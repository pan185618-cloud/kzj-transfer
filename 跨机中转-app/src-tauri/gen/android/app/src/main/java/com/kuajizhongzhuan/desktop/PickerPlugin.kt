package com.kuajizhongzhuan.desktop

import android.app.Activity
import android.content.Intent
import android.graphics.Bitmap
import android.graphics.BitmapFactory
import android.net.Uri
import android.provider.DocumentsContract
import android.provider.OpenableColumns
import android.util.Base64
import android.webkit.MimeTypeMap
import androidx.core.content.FileProvider
import androidx.activity.result.ActivityResult
import app.tauri.annotation.ActivityCallback
import app.tauri.annotation.Command
import app.tauri.annotation.InvokeArg
import app.tauri.annotation.TauriPlugin
import app.tauri.plugin.Invoke
import app.tauri.plugin.JSArray
import app.tauri.plugin.JSObject
import app.tauri.plugin.Plugin
import java.io.ByteArrayOutputStream
import java.io.File
import java.net.HttpURLConnection
import java.net.URL
import org.json.JSONObject

@InvokeArg
class LocalFileArgs {
  lateinit var path: String
  var mimeType: String? = null
}

@InvokeArg
class UpdateManifestArgs {
  lateinit var urls: Array<String>
}

// 系统文件选择器(SAF)。tauri-plugin-dialog 在 Android 上只返回裸 content:// URI,
// 且不支持选目录;这里自己发 Intent,把每个文件的 uri/文件名/大小/MIME 一起返回。
// 选文件夹走 ACTION_OPEN_DOCUMENT_TREE,递归遍历子文档,name 用 "文件夹/子路径/文件"
// 的相对路径,和桌面端 collect_path 的命名一致,接收端会按文件夹分组。
@TauriPlugin
class PickerPlugin(private val activity: Activity) : Plugin(activity) {

  private fun localUri(path: String): Uri =
    if (path.startsWith("content://")) Uri.parse(path) else Uri.fromFile(File(path))

  // 使用系统解码器生成小图,兼容 SAF URI、HEIC 和大尺寸照片,避免把原图塞进 WebView。
  @Command
  fun readImagePreview(invoke: Invoke) {
    val args = invoke.parseArgs(LocalFileArgs::class.java)
    Thread {
      try {
        val uri = localUri(args.path)
        val resolver = activity.contentResolver
        val bounds = BitmapFactory.Options().apply { inJustDecodeBounds = true }
        resolver.openInputStream(uri)?.use { BitmapFactory.decodeStream(it, null, bounds) }
        require(bounds.outWidth > 0 && bounds.outHeight > 0) { "无法解码这张图片" }
        val options = BitmapFactory.Options().apply { inSampleSize = 1 }
        while (maxOf(bounds.outWidth, bounds.outHeight) / options.inSampleSize > 960) options.inSampleSize *= 2
        val bitmap = resolver.openInputStream(uri)?.use { BitmapFactory.decodeStream(it, null, options) }
          ?: throw IllegalArgumentException("图片不可读取,请重新选择文件")
        val out = ByteArrayOutputStream()
        val alpha = bitmap.hasAlpha()
        try {
          check(bitmap.compress(if (alpha) Bitmap.CompressFormat.PNG else Bitmap.CompressFormat.JPEG, 85, out)) { "生成预览失败" }
        } finally { bitmap.recycle() }
        val ret = JSObject()
        ret.put("url", "data:image/${if (alpha) "png" else "jpeg"};base64," + Base64.encodeToString(out.toByteArray(), Base64.NO_WRAP))
        invoke.resolve(ret)
      } catch (ex: Exception) { invoke.reject(ex.message ?: "图片预览失败") }
    }.start()
  }

  @Command
  fun openFile(invoke: Invoke) {
    val args = invoke.parseArgs(LocalFileArgs::class.java)
    try {
      val uri = if (args.path.startsWith("content://")) Uri.parse(args.path)
        else FileProvider.getUriForFile(activity, "${activity.packageName}.fileprovider", File(args.path))
      val mime = args.mimeType?.takeIf { it != "application/octet-stream" && it.isNotBlank() }
        ?: activity.contentResolver.getType(uri)
        ?: MimeTypeMap.getSingleton().getMimeTypeFromExtension(File(args.path).extension.lowercase())
        ?: "application/octet-stream"
      val intent = Intent(Intent.ACTION_VIEW).apply {
        setDataAndType(uri, mime)
        addFlags(Intent.FLAG_GRANT_READ_URI_PERMISSION)
        clipData = android.content.ClipData.newRawUri("文件", uri)
      }
      activity.startActivity(intent)
      invoke.resolve()
    } catch (ex: android.content.ActivityNotFoundException) {
      invoke.reject("手机没有能打开此格式的应用")
    } catch (ex: Exception) { invoke.reject(ex.message ?: "文件无法打开,请重新选择文件") }
  }

  // Android 的系统 HTTPS 已初始化证书环境;桌面 Rust 客户端不能直接复用到这里。
  @Command
  fun fetchUpdateManifest(invoke: Invoke) {
    val args = invoke.parseArgs(UpdateManifestArgs::class.java)
    Thread {
      val errors = mutableListOf<String>()
      for (address in args.urls) {
        var connection: HttpURLConnection? = null
        try {
          val url = URL(address)
          require(url.protocol == "https") { "更新地址必须使用 HTTPS" }
          connection = url.openConnection() as HttpURLConnection
          connection.connectTimeout = 5000
          connection.readTimeout = 5000
          connection.useCaches = false
          connection.setRequestProperty("Accept", "application/json")
          check(connection.responseCode in 200..299) { "HTTP ${connection.responseCode}" }
          val bytes = connection.inputStream.use { stream ->
            val out = ByteArrayOutputStream()
            val buffer = ByteArray(8192)
            while (out.size() <= 256 * 1024) {
              val n = stream.read(buffer)
              if (n < 0) break
              out.write(buffer, 0, n)
            }
            out.toByteArray()
          }
          require(bytes.size <= 256 * 1024) { "更新清单过大" }
          val json = bytes.toString(Charsets.UTF_8)
          require(JSONObject(json).optString("version").isNotBlank()) { "更新清单缺少 version" }
          val ret = JSObject()
          ret.put("json", json)
          invoke.resolve(ret)
          return@Thread
        } catch (ex: Exception) { errors.add("${URL(address).host}: ${ex.message ?: "连接失败"}") }
        finally { connection?.disconnect() }
      }
      invoke.reject("检查更新失败: " + errors.joinToString("; "))
    }.start()
  }

  private fun keepReadAccess(uri: Uri, flags: Int) {
    try {
      if (flags and Intent.FLAG_GRANT_READ_URI_PERMISSION != 0) {
        activity.contentResolver.takePersistableUriPermission(uri, Intent.FLAG_GRANT_READ_URI_PERMISSION)
      }
    } catch (_: SecurityException) { /* 部分来源只授予本次访问,仍允许本次传输。 */ }
  }

  @Command
  fun pickFiles(invoke: Invoke) {
    val intent = Intent(Intent.ACTION_OPEN_DOCUMENT).apply {
      addCategory(Intent.CATEGORY_OPENABLE)
      type = "*/*"
      putExtra(Intent.EXTRA_ALLOW_MULTIPLE, true)
    }
    startActivityForResult(invoke, intent, "filesResult")
  }

  @Command
  fun pickFolder(invoke: Invoke) {
    startActivityForResult(invoke, Intent(Intent.ACTION_OPEN_DOCUMENT_TREE), "folderResult")
  }

  @ActivityCallback
  fun filesResult(invoke: Invoke, result: ActivityResult) {
    val data = result.data
    if (result.resultCode != Activity.RESULT_OK || data == null) {
      resolveItems(invoke, JSArray())
      return
    }
    val uris = mutableListOf<Uri>()
    val clip = data.clipData
    if (clip != null) {
      for (i in 0 until clip.itemCount) uris.add(clip.getItemAt(i).uri)
    } else {
      data.data?.let { uris.add(it) }
    }
    for (uri in uris) keepReadAccess(uri, data.flags)
    Thread {
      try {
        val items = JSArray()
        for (uri in uris) items.put(describe(uri))
        resolveItems(invoke, items)
      } catch (ex: Exception) {
        invoke.reject(ex.message ?: "读取所选文件失败")
      }
    }.start()
  }

  @ActivityCallback
  fun folderResult(invoke: Invoke, result: ActivityResult) {
    val tree = result.data?.data
    if (result.resultCode != Activity.RESULT_OK || tree == null) {
      resolveItems(invoke, JSArray())
      return
    }
    keepReadAccess(tree, result.data?.flags ?: 0)
    // 大目录遍历可能很慢,别卡主线程
    Thread {
      try {
        val rootId = DocumentsContract.getTreeDocumentId(tree)
        val rootUri = DocumentsContract.buildDocumentUriUsingTree(tree, rootId)
        val rootName = queryName(rootUri) ?: rootId.substringAfterLast(':').substringAfterLast('/')
        val items = JSArray()
        walk(tree, rootId, rootName.ifEmpty { "文件夹" }, items)
        resolveItems(invoke, items)
      } catch (ex: Exception) {
        invoke.reject(ex.message ?: "读取文件夹失败")
      }
    }.start()
  }

  private fun resolveItems(invoke: Invoke, items: JSArray) {
    val ret = JSObject()
    ret.put("items", items)
    invoke.resolve(ret)
  }

  private fun walk(tree: Uri, parentId: String, prefix: String, out: JSArray) {
    val children = DocumentsContract.buildChildDocumentsUriUsingTree(tree, parentId)
    val cols = arrayOf(
      DocumentsContract.Document.COLUMN_DOCUMENT_ID,
      DocumentsContract.Document.COLUMN_DISPLAY_NAME,
      DocumentsContract.Document.COLUMN_MIME_TYPE,
      DocumentsContract.Document.COLUMN_SIZE,
    )
    val rows = mutableListOf<Array<Any?>>()
    activity.contentResolver.query(children, cols, null, null, null)?.use { c ->
      while (c.moveToNext()) {
        rows.add(arrayOf(
          c.getString(0),
          c.getString(1),
          c.getString(2),
          if (c.isNull(3)) null else c.getLong(3),
        ))
      }
    }
    rows.sortBy { (it[1] as String?) ?: "" }
    for (row in rows) {
      val id = row[0] as String? ?: continue
      val name = row[1] as String? ?: id.substringAfterLast('/')
      val mime = row[2] as String?
      if (mime == DocumentsContract.Document.MIME_TYPE_DIR) {
        walk(tree, id, "$prefix/$name", out)
      } else {
        val item = JSObject()
        item.put("uri", DocumentsContract.buildDocumentUriUsingTree(tree, id).toString())
        item.put("name", "$prefix/$name")
        item.put("size", (row[3] as Long?) ?: 0L)
        item.put("mimeType", mime)
        out.put(item)
      }
    }
  }

  private fun describe(uri: Uri): JSObject {
    var name: String? = null
    var size: Long? = null
    activity.contentResolver.query(
      uri, arrayOf(OpenableColumns.DISPLAY_NAME, OpenableColumns.SIZE), null, null, null
    )?.use { c ->
      if (c.moveToFirst()) {
        name = c.getString(0)
        if (!c.isNull(1)) size = c.getLong(1)
      }
    }
    val item = JSObject()
    item.put("uri", uri.toString())
    item.put("name", name ?: (uri.lastPathSegment ?: "file").substringAfterLast('/'))
    item.put("size", size ?: 0L)
    item.put("mimeType", activity.contentResolver.getType(uri))
    return item
  }

  private fun queryName(uri: Uri): String? =
    activity.contentResolver.query(
      uri, arrayOf(DocumentsContract.Document.COLUMN_DISPLAY_NAME), null, null, null
    )?.use { c -> if (c.moveToFirst()) c.getString(0) else null }
}
