package com.kuajizhongzhuan.desktop

import android.Manifest
import android.app.Activity
import android.content.ContentUris
import android.content.Intent
import android.content.pm.PackageManager
import android.database.ContentObserver
import android.net.Uri
import android.os.Build
import android.os.Handler
import android.os.Looper
import android.provider.MediaStore
import android.provider.OpenableColumns
import android.view.DragEvent
import android.view.View
import android.webkit.WebView
import androidx.core.content.FileProvider
import app.tauri.plugin.JSObject
import java.io.File
import java.util.UUID
import java.util.concurrent.Executors
import org.json.JSONArray
import org.json.JSONObject

/** 截图建议只查最近三分钟。拖入/分享的临时 URI 先复制到应用目录，保证发送和历史预览可用。 */
class QuickSend(private val activity: Activity) {
  private val prefs = activity.getSharedPreferences("quick-send", Activity.MODE_PRIVATE)
  private val lock = Any()
  private val worker = Executors.newSingleThreadExecutor()
  private val handler = Handler(Looper.getMainLooper())
  private var webView: WebView? = null
  private var observing = false
  private val changed = Runnable { signal() }
  private val observer = object : ContentObserver(handler) {
    override fun onChange(selfChange: Boolean) {
      // 截图保存时会分多次更新 MediaStore，等写入完成再查询。
      handler.removeCallbacks(changed)
      handler.postDelayed(changed, 600)
    }
  }

  fun attach(view: WebView) {
    webView = view
    val listener = View.OnDragListener { _, event ->
      when (event.action) {
        DragEvent.ACTION_DRAG_STARTED -> {
          val description = event.clipDescription
          android.util.Log.i("KzjQuickSend", "Image drag entered window")
          description != null && (description.hasMimeType("image/*") ||
            description.hasMimeType("text/uri-list") || description.hasMimeType("application/octet-stream"))
        }
        DragEvent.ACTION_DRAG_ENTERED -> { dragHighlight(true); true }
        DragEvent.ACTION_DRAG_EXITED, DragEvent.ACTION_DRAG_ENDED -> { dragHighlight(false); true }
        DragEvent.ACTION_DRAG_LOCATION -> true
        DragEvent.ACTION_DROP -> {
          android.util.Log.i("KzjQuickSend", "Received image drop")
          dragHighlight(false)
          val clip = event.clipData
          val uris = (0 until (clip?.itemCount ?: 0)).mapNotNull { clip?.getItemAt(it)?.uri }
            .filter { it.scheme == "content" }.distinct()
          if (uris.isEmpty()) false else {
            val permission = activity.requestDragAndDropPermissions(event)
            importImages(uris, "drop") { permission?.release() }
            true
          }
        }
        else -> false
      }
    }
    // 部分 WebView 的内部子视图会先接住 drag-start；窗口根视图也作为接收区。
    view.setOnDragListener(listener)
    activity.window.decorView.setOnDragListener(listener)
    receive(activity.intent)
  }

  fun receive(intent: Intent) {
    if (intent.action != Intent.ACTION_SEND && intent.action != Intent.ACTION_SEND_MULTIPLE) return
    if (intent.type?.startsWith("image/") != true) return
    @Suppress("DEPRECATION")
    val streams = if (intent.action == Intent.ACTION_SEND_MULTIPLE)
      intent.getParcelableArrayListExtra<Uri>(Intent.EXTRA_STREAM).orEmpty()
    else listOfNotNull(intent.getParcelableExtra<Uri>(Intent.EXTRA_STREAM))
    val clip = intent.clipData
    val uris = (streams + (0 until (clip?.itemCount ?: 0)).mapNotNull { clip?.getItemAt(it)?.uri })
      .filter { it.scheme == "content" }.distinct()
    // singleTask 的 Intent 会保留到下次启动，消费后清掉，避免一张图重复发送。
    intent.action = Intent.ACTION_MAIN
    intent.removeExtra(Intent.EXTRA_STREAM)
    intent.clipData = null
    if (uris.isNotEmpty()) importImages(uris, "share")
  }

  private fun dragHighlight(active: Boolean) {
    webView?.evaluateJavascript("window.dispatchEvent(new CustomEvent('android-image-drag',{detail:$active}))", null)
  }

  private fun signal() {
    webView?.post {
      webView?.evaluateJavascript("window.dispatchEvent(new Event('android-quick-change'))", null)
    }
  }

  fun resume() {
    if (!observing) {
      activity.contentResolver.registerContentObserver(MediaStore.Images.Media.EXTERNAL_CONTENT_URI, true, observer)
      observing = true
    }
    signal()
  }

  fun pause() {
    if (observing) activity.contentResolver.unregisterContentObserver(observer)
    observing = false
    handler.removeCallbacks(changed)
  }

  fun destroy() { pause(); webView = null; worker.shutdown() }

  fun enabled() = prefs.getBoolean("enabled", false)
  fun setEnabled(enabled: Boolean) { prefs.edit().putBoolean("enabled", enabled).apply() }

  fun access(): String = when {
    activity.checkSelfPermission(if (Build.VERSION.SDK_INT >= 33) Manifest.permission.READ_MEDIA_IMAGES
      else Manifest.permission.READ_EXTERNAL_STORAGE) == PackageManager.PERMISSION_GRANTED -> "full"
    Build.VERSION.SDK_INT >= 34 && activity.checkSelfPermission(Manifest.permission.READ_MEDIA_VISUAL_USER_SELECTED) == PackageManager.PERMISSION_GRANTED -> "limited"
    else -> "none"
  }

  private fun screenshotKey(value: String): String {
    val uri = Uri.parse(value)
    val id = uri.lastPathSegment
    // MediaStore 的 external / external_primary 是同一张图的不同入口。
    return if (uri.authority == "media" && "images" in uri.pathSegments && id?.toLongOrNull() != null)
      "media-image:$id" else value
  }

  fun state(): JSObject = JSObject().apply {
    put("enabled", enabled())
    put("access", access())
    put("imports", synchronized(lock) { JSONArray(prefs.getString("imports", "[]")) })
    put("screenshot", if (enabled() && access() != "none") recentScreenshot() else JSONObject.NULL)
  }

  fun acknowledge(id: String?, uri: String?) {
    synchronized(lock) {
      val edit = prefs.edit()
      if (id != null) {
        val old = JSONArray(prefs.getString("imports", "[]"))
        val remaining = JSONArray()
        for (i in 0 until old.length()) if (old.getJSONObject(i).optString("id") != id) remaining.put(old.get(i))
        edit.putString("imports", remaining.toString())
      }
      if (uri != null) {
        val old = JSONArray(prefs.getString("dismissed", "[]"))
        val key = screenshotKey(uri)
        val recent = (0 until old.length()).map { screenshotKey(old.getString(it)) }.filter { it != key }.takeLast(39) + key
        edit.putString("dismissed", JSONArray(recent).toString())
      }
      edit.apply()
    }
  }

  private fun recentScreenshot(): JSONObject? {
    val now = System.currentTimeMillis()
    val columns = mutableListOf(MediaStore.Images.Media._ID, MediaStore.Images.Media.DISPLAY_NAME,
      MediaStore.Images.Media.SIZE, MediaStore.Images.Media.MIME_TYPE, MediaStore.Images.Media.DATE_ADDED,
      MediaStore.Images.Media.DATE_TAKEN, MediaStore.Images.Media.BUCKET_DISPLAY_NAME)
    if (Build.VERSION.SDK_INT >= 29) columns.add(MediaStore.Images.Media.RELATIVE_PATH)
    val dismissed = JSONArray(prefs.getString("dismissed", "[]"))
    val skipped = (0 until dismissed.length()).map { screenshotKey(dismissed.getString(it)) }.toSet()
    try {
      val selection = "${MediaStore.Images.Media.DATE_ADDED} >= ?" +
        if (Build.VERSION.SDK_INT >= 29) " AND ${MediaStore.Images.Media.IS_PENDING} = 0" else ""
      activity.contentResolver.query(MediaStore.Images.Media.EXTERNAL_CONTENT_URI, columns.toTypedArray(),
        selection, arrayOf(((now - 180_000) / 1000).toString()),
        "${MediaStore.Images.Media.DATE_ADDED} DESC, ${MediaStore.Images.Media._ID} DESC")?.use { cursor ->
        var checked = 0
        while (cursor.moveToNext() && checked++ < 40) {
          val uri = ContentUris.withAppendedId(MediaStore.Images.Media.EXTERNAL_CONTENT_URI, cursor.getLong(0)).toString()
          if (screenshotKey(uri) in skipped || cursor.getLong(2) <= 0) continue
          val taken = cursor.getLong(5).takeIf { it > 0 } ?: (cursor.getLong(4) * 1000)
          if (taken < now - 180_000 || taken > now + 10_000) continue
          val name = cursor.getString(1) ?: "截图.png"
          val path = "$name ${cursor.getString(6).orEmpty()} ${if (Build.VERSION.SDK_INT >= 29) cursor.getString(7).orEmpty() else ""}".lowercase()
          if (listOf("screenshot", "screen_shot", "screen-shot", "screen capture", "screen_capture", "screencapture", "截屏", "截图", "截圖", "螢幕擷取").none { path.contains(it) }) continue
          return JSONObject().put("uri", uri).put("name", name).put("size", cursor.getLong(2))
            .put("mimeType", cursor.getString(3) ?: "image/png").put("expiresAt", taken + 180_000)
        }
      }
    } catch (_: SecurityException) { /* 用户可随时撤销照片权限。 */ }
    return null
  }

  private fun importImages(uris: List<Uri>, source: String, release: () -> Unit = {}) {
    worker.execute {
      val items = JSONArray()
      val errors = mutableListOf<String>()
      try {
        for (uri in uris) {
          var target: File? = null
          try {
            val mime = activity.contentResolver.getType(uri) ?: "image/png"
            require(mime.startsWith("image/")) { "请拖入或分享图片" }
            var name = "截图.${if (mime == "image/jpeg") "jpg" else "png"}"
            activity.contentResolver.query(uri, arrayOf(OpenableColumns.DISPLAY_NAME), null, null, null)?.use { c ->
              if (c.moveToFirst()) name = c.getString(0) ?: name
            }
            val originalName = name
            name = name.replace('\\', '/').substringAfterLast('/').replace(Regex("[\\p{Cntrl}]"), "_")
              .take(80).takeUnless { it.isBlank() || it == "." || it == ".." } ?: "截图.png"
            val dir = File(activity.filesDir, "quick-send/${UUID.randomUUID()}").apply { check(mkdirs()) }
            val saved = File(dir, name)
            target = saved
            activity.contentResolver.openInputStream(uri)?.use { input -> saved.outputStream().use { input.copyTo(it) } }
              ?: error("截图已失效，请重新分享")
            check(saved.length() > 0) { "截图还没有保存完成，请稍后重试" }
            val savedUri = FileProvider.getUriForFile(activity, "${activity.packageName}.fileprovider", saved)
            items.put(JSONObject().put("uri", savedUri.toString()).put("name", name)
              .put("size", saved.length()).put("mimeType", mime))
            acknowledge(null, uri.toString())
            // OPPO 等系统用自己的 FileProvider 分享截图，和媒体库的 URI 不同。
            // 只有文件名和大小均匹配才收起对应建议，不影响另一张刚截好的图。
            if (enabled() && access() != "none") {
              val candidate = recentScreenshot()
              if (candidate?.optString("name") == originalName && candidate.optLong("size") == saved.length())
                acknowledge(null, candidate.getString("uri"))
            }
          } catch (ex: Exception) { target?.delete(); errors.add(ex.message ?: "读取图片失败") }
        }
        synchronized(lock) {
          val pending = JSONArray(prefs.getString("imports", "[]"))
          pending.put(JSONObject().put("id", UUID.randomUUID().toString()).put("source", source)
            .put("items", items).put("error", errors.distinct().joinToString("；")))
          // 分享来源的临时权限即将释放，先持久保存队列。
          prefs.edit().putString("imports", pending.toString()).commit()
        }
        android.util.Log.i("KzjQuickSend", "Imported ${items.length()} image(s) from $source")
      } finally { release(); signal() }
    }
  }
}
