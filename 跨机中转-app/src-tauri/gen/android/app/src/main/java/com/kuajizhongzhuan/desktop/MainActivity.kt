package com.kuajizhongzhuan.desktop

import android.content.Context
import android.net.wifi.WifiManager
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
  }

  override fun onDestroy() {
    multicastLock?.let {
      if (it.isHeld) it.release()
    }
    multicastLock = null
    super.onDestroy()
  }
}
