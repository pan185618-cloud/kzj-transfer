package com.kuajizhongzhuan.desktop

import android.app.Activity
import android.content.Intent
import android.net.Uri
import android.provider.DocumentsContract
import android.provider.OpenableColumns
import androidx.activity.result.ActivityResult
import app.tauri.annotation.ActivityCallback
import app.tauri.annotation.Command
import app.tauri.annotation.TauriPlugin
import app.tauri.plugin.Invoke
import app.tauri.plugin.JSArray
import app.tauri.plugin.JSObject
import app.tauri.plugin.Plugin

// 系统文件选择器(SAF)。tauri-plugin-dialog 在 Android 上只返回裸 content:// URI,
// 且不支持选目录;这里自己发 Intent,把每个文件的 uri/文件名/大小/MIME 一起返回。
// 选文件夹走 ACTION_OPEN_DOCUMENT_TREE,递归遍历子文档,name 用 "文件夹/子路径/文件"
// 的相对路径,和桌面端 collect_path 的命名一致,接收端会按文件夹分组。
@TauriPlugin
class PickerPlugin(private val activity: Activity) : Plugin(activity) {

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
