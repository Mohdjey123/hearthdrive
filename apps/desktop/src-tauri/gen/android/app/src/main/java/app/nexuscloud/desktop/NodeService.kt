package app.nexuscloud.desktop

import android.app.Notification
import android.app.NotificationChannel
import android.app.NotificationManager
import android.app.PendingIntent
import android.app.Service
import android.content.BroadcastReceiver
import android.content.Context
import android.content.Intent
import android.content.pm.ServiceInfo
import android.os.Build
import android.os.Handler
import android.os.IBinder
import android.os.Looper
import android.os.Process
import android.util.Log
import androidx.core.app.NotificationCompat
import androidx.core.app.ServiceCompat
import androidx.core.content.ContextCompat
import java.io.File

/**
 * Keeps this phone's HearthDrive node running when the app is not open.
 *
 * The node runs in this service's own process (`:node` in the manifest),
 * not the window's: Tauri ends the window's process when its screen is
 * closed or swiped away. The service is a foreground service with a
 * notification, so Android keeps it running; the notification's Quit stops
 * the node, and so does force-stopping the app. The window finds the node
 * through the address file the node writes in the app's data folder.
 */
class NodeService : Service() {
  companion object {
    private const val TAG = "NexusNode"
    private const val CHANNEL = "node"
    private const val NOTIFICATION_ID = 1
    private const val ACTION_QUIT = "app.nexuscloud.desktop.action.QUIT"

    /** Sent to the window when Quit is tapped, so it closes too. */
    const val ACTION_QUITTING = "app.nexuscloud.desktop.action.QUITTING"

    /** Sent to the window when the service could not run in the foreground. */
    const val ACTION_UNAVAILABLE = "app.nexuscloud.desktop.action.NODE_UNAVAILABLE"

    private const val PREFS = "node"
    private const val KEY_RUNNING = "running"

    init {
      System.loadLibrary("nexus_app")
    }

    /** Starts the node on its own thread in the calling process (see lib.rs). */
    @JvmStatic external fun startNode(context: Context, dataDir: String, shareInbox: String, ownProcess: Boolean)

    /** Starts the service, which starts the node. False if Android refused. */
    fun start(context: Context): Boolean =
      try {
        ContextCompat.startForegroundService(context, Intent(context, NodeService::class.java))
        true
      } catch (e: Exception) {
        Log.w(TAG, "Could not start the background service: $e")
        false
      }

    /**
     * Runs the node inside the window's process instead, as before there was
     * a service: it then stops when the app is closed.
     */
    fun startInProcess(context: Context) {
      val app = context.applicationContext
      startNode(app, dataDir(app), shareInbox(app), false)
    }

    /** Whether the node was running before the app was updated. */
    fun wasRunning(context: Context): Boolean =
      context.getSharedPreferences(PREFS, MODE_PRIVATE).getBoolean(KEY_RUNNING, false)

    private fun setRunning(context: Context, running: Boolean) {
      context.getSharedPreferences(PREFS, MODE_PRIVATE).edit().putBoolean(KEY_RUNNING, running).apply()
    }

    // The same folders Tauri gives the window as app_data_dir and app_cache_dir.
    private fun dataDir(context: Context): String = context.dataDir.absolutePath
    private fun shareInbox(context: Context): String = File(context.cacheDir, "shared").absolutePath
  }

  override fun onBind(intent: Intent?): IBinder? = null

  private var nodeStarted = false

  override fun onStartCommand(intent: Intent?, flags: Int, startId: Int): Int {
    if (intent?.action == ACTION_QUIT) {
      quit()
      return START_NOT_STICKY
    }
    // Each start from the app must be answered by running in the foreground.
    try {
      val type = if (Build.VERSION.SDK_INT >= 34) ServiceInfo.FOREGROUND_SERVICE_TYPE_SPECIAL_USE else 0
      ServiceCompat.startForeground(this, NOTIFICATION_ID, notification(), type)
    } catch (e: Exception) {
      // Android would not let the service run in the foreground. The window
      // then runs the node itself, while the app is open.
      Log.w(TAG, "Could not run in the foreground: $e")
      if (!nodeStarted) {
        sendBroadcast(Intent(ACTION_UNAVAILABLE).setPackage(packageName))
        stopSelf()
        Handler(Looper.getMainLooper()).postDelayed({ Process.killProcess(Process.myPid()) }, 300)
      }
      return START_NOT_STICKY
    }
    if (!nodeStarted) {
      nodeStarted = true
      setRunning(this, true)
      startNode(applicationContext, dataDir(this), shareInbox(this), true)
    }
    // If Android ends the process to free memory, it starts the service
    // (and so the node) again when it can.
    return START_STICKY
  }

  /** Swiping the app away closes only its window; the node keeps running. */
  override fun onTaskRemoved(rootIntent: Intent?) {
    super.onTaskRemoved(rootIntent)
  }

  /**
   * Quit: closes the window, asks the node to shut down cleanly (it stops
   * on SIGINT, like Ctrl+C on a computer), and ends this process.
   */
  private fun quit() {
    setRunning(this, false)
    sendBroadcast(Intent(ACTION_QUITTING).setPackage(packageName))
    ServiceCompat.stopForeground(this, ServiceCompat.STOP_FOREGROUND_REMOVE)
    stopSelf()
    Process.sendSignal(Process.myPid(), 2 /* SIGINT */)
    Handler(Looper.getMainLooper()).postDelayed({ Process.killProcess(Process.myPid()) }, 3000)
  }

  private fun notification(): Notification {
    val manager = getSystemService(NotificationManager::class.java)
    if (Build.VERSION.SDK_INT >= 26 && manager.getNotificationChannel(CHANNEL) == null) {
      manager.createNotificationChannel(
        NotificationChannel(CHANNEL, getString(R.string.node_channel), NotificationManager.IMPORTANCE_LOW).apply {
          description = getString(R.string.node_channel_description)
          setShowBadge(false)
        }
      )
    }
    val immutable = if (Build.VERSION.SDK_INT >= 23) PendingIntent.FLAG_IMMUTABLE else 0
    val open = PendingIntent.getActivity(
      this, 0,
      Intent(this, MainActivity::class.java).addFlags(Intent.FLAG_ACTIVITY_NEW_TASK or Intent.FLAG_ACTIVITY_SINGLE_TOP),
      immutable or PendingIntent.FLAG_UPDATE_CURRENT
    )
    val quit = PendingIntent.getService(
      this, 1,
      Intent(this, NodeService::class.java).setAction(ACTION_QUIT),
      immutable or PendingIntent.FLAG_UPDATE_CURRENT
    )
    return NotificationCompat.Builder(this, CHANNEL)
      .setSmallIcon(R.drawable.ic_stat_hearthdrive)
      .setContentTitle(getString(R.string.node_running))
      .setContentText(getString(R.string.node_running_text))
      .setContentIntent(open)
      .addAction(0, getString(R.string.node_quit), quit)
      .setOngoing(true)
      .setShowWhen(false)
      .setCategory(NotificationCompat.CATEGORY_SERVICE)
      .setPriority(NotificationCompat.PRIORITY_LOW)
      .setForegroundServiceBehavior(NotificationCompat.FOREGROUND_SERVICE_IMMEDIATE)
      .build()
  }
}

/**
 * Starts the node again after HearthDrive updates itself (the update ends
 * every process of the app), if it was running before. Android lets a
 * foreground service start from this broadcast.
 */
class PackageReplacedReceiver : BroadcastReceiver() {
  override fun onReceive(context: Context, intent: Intent) {
    if (intent.action == Intent.ACTION_MY_PACKAGE_REPLACED && NodeService.wasRunning(context)) {
      NodeService.start(context)
    }
  }
}
