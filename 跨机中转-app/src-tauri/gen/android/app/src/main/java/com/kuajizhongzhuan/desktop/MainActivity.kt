package com.kuajizhongzhuan.desktop

import android.content.Context
import android.content.pm.PackageManager
import android.net.wifi.WifiManager
import android.os.Build
import android.os.Bundle
import androidx.activity.enableEdgeToEdge

class MainActivity : TauriActivity() {
  private var multicastLock: WifiManager.MulticastLock? = null

  override fun onCreate(savedInstanceState: Bundle?) {
    enableEdgeToEdge()
    // LocalSend discovery relies on UDP multicast; Android requires holding a
    // MulticastLock or the kernel drops the multicast packets.
    val wifi = applicationContext.getSystemService(Context.WIFI_SERVICE) as WifiManager
    multicastLock = wifi.createMulticastLock("kuajizhongzhuan").apply {
      setReferenceCounted(true)
      acquire()
    }
    super.onCreate(savedInstanceState)
    requestLocalNetworkAccess()
  }

  // Android 17(API 37)起局域网访问是运行时权限。没有它,手机发出去的请求还能走,
  // 但别的设备连进来时,本机的回包会被系统丢掉,导致收不到文件。
  private fun requestLocalNetworkAccess() {
    if (Build.VERSION.SDK_INT < 37) return
    val perm = "android.permission.ACCESS_LOCAL_NETWORK"
    if (checkSelfPermission(perm) != PackageManager.PERMISSION_GRANTED) {
      requestPermissions(arrayOf(perm), REQ_LOCAL_NETWORK)
    }
  }

  companion object {
    private const val REQ_LOCAL_NETWORK = 4317
  }

  override fun onDestroy() {
    multicastLock?.let {
      if (it.isHeld) it.release()
    }
    multicastLock = null
    super.onDestroy()
  }
}
