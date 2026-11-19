package app.nexuscloud.desktop

import android.Manifest
import android.app.PendingIntent
import android.content.BroadcastReceiver
import android.content.ContentResolver
import android.content.Context
import android.content.Intent
import android.content.pm.PackageInstaller
import android.content.pm.PackageManager
import android.graphics.Color
import android.net.Uri
import android.os.Build
import android.os.Bundle
import android.provider.OpenableColumns
import android.provider.Settings
import android.util.Log
import android.webkit.JavascriptInterface
import android.webkit.WebView
import androidx.activity.SystemBarStyle
import androidx.activity.enableEdgeToEdge
import androidx.core.content.ContextCompat
import java.io.File

/**
 * The app's only screen. Besides hosting the web view it:
 *
 * - asks for permission to read photos and videos, for backup, through a
 *   small `window.NexusAndroid` bridge the page can call;
 * - receives files shared to HearthDrive from other apps (the share menu) and
 *   copies them into `<cache>/shared`, where the node sends them from once
 *   the person picks a computer and folder;
 * - hands an update the node received and verified to Android's package
 *   installer: when the person taps Install, or by itself once the app goes
 *   to the background (Android 12 and later lets an app update itself
 *   without asking).
 */
class MainActivity : TauriActivity() {
  private var webView: WebView? = null

  /** The verified update to install once the app is in the background. */
  @Volatile private var quietUpdate: String? = null

  override fun onCreate(savedInstanceState: Bundle?) {
    // The app draws its own dark background under the system bars, so the
    // status and navigation bar icons are always light.
    enableEdgeToEdge(
      statusBarStyle = SystemBarStyle.dark(Color.TRANSPARENT),
      navigationBarStyle = SystemBarStyle.dark(Color.TRANSPARENT)
    )
    super.onCreate(savedInstanceState)
    // A recreated activity (after rotation, say) already took its share.
    if (savedInstanceState == null) receiveShare(intent)
  }

  override fun onNewIntent(intent: Intent) {
    super.onNewIntent(intent)
    setIntent(intent)
    receiveShare(intent)
  }

  override fun onStop() {
    super.onStop()
    // Out of sight: install a waiting update by itself, if the page asked
    // for that and Android allows it without asking. The app is closed
    // while it installs; the new version starts when it is next opened.
    val path = quietUpdate ?: return
    if (!canUpdateQuietly()) return
    quietUpdate = null
    Thread { installUpdate(path, interactive = false) }.start()
  }

  override fun onWebViewCreate(webView: WebView) {
    this.webView = webView
    webView.addJavascriptInterface(Bridge(), "NexusAndroid")
  }

  /** Tells the page something changed, as a DOM event on `window`. */
  private fun notifyPage(event: String) {
    runOnUiThread { webView?.evaluateJavascript("window.dispatchEvent(new Event('$event'))", null) }
  }

  // -- Photos and videos ---------------------------------------------------

  private fun mediaPermissions(): Array<String> =
    if (Build.VERSION.SDK_INT >= 33) {
      arrayOf(Manifest.permission.READ_MEDIA_IMAGES, Manifest.permission.READ_MEDIA_VIDEO)
    } else {
      arrayOf(Manifest.permission.READ_EXTERNAL_STORAGE)
    }

  private fun hasMediaAccess(): Boolean =
    mediaPermissions().all { ContextCompat.checkSelfPermission(this, it) == PackageManager.PERMISSION_GRANTED }

  inner class Bridge {
    @JavascriptInterface
    fun hasMediaAccess(): Boolean = this@MainActivity.hasMediaAccess()

    /** Shows Android's permission prompt; the page hears `nexus-media-access` after. */
    @JavascriptInterface
    fun requestMediaAccess() {
      runOnUiThread {
        this@MainActivity.requestPermissions(mediaPermissions()) { notifyPage("nexus-media-access") }
      }
    }

    /**
     * Opens Android's installer for the update package at [path], which the
     * node verified (signature, size and hash) before keeping it. Only the
     * node's own update package is accepted. Returns `started`,
     * `permission` (HearthDrive must first be allowed to install apps; the
     * setting is opened) or `missing`.
     */
    @JavascriptInterface
    fun installUpdate(path: String): String = this@MainActivity.installUpdate(path, interactive = true)

    /** The update to install once the app is in the background ('' for none). */
    @JavascriptInterface
    fun installInBackground(path: String) {
      quietUpdate = path.ifEmpty { null }
    }

    /** Whether updates can install without asking (Android 12 and later, once allowed). */
    @JavascriptInterface
    fun canUpdateQuietly(): Boolean = this@MainActivity.canUpdateQuietly()

    /** Opens this app's page in Android settings (when the prompt was turned off). */
    @JavascriptInterface
    fun openAppSettings() {
      runOnUiThread {
        startActivity(Intent(Settings.ACTION_APPLICATION_DETAILS_SETTINGS, Uri.fromParts("package", packageName, null)))
      }
    }
  }

  // -- Updates -------------------------------------------------------------

  /** Android 12 and later, with HearthDrive allowed to install apps. */
  private fun canUpdateQuietly(): Boolean =
    Build.VERSION.SDK_INT >= 31 && packageManager.canRequestPackageInstalls()

  /** The node's own verified update package, if [path] is it. */
  private fun updatePackage(path: String): File? {
    val file = try { File(path).canonicalFile } catch (e: Exception) { return null }
    val root = File(applicationInfo.dataDir).canonicalFile
    val expected = "updates" + File.separator + "android-aarch64" + File.separator + "package.apk"
    if (!file.path.startsWith(root.path + File.separator) || !file.path.endsWith(File.separator + expected) || !file.isFile) {
      return null
    }
    return file
  }

  /**
   * Hands the update at [path] to Android's package installer, in a
   * session that asks Android not to need the person (Android 12 and
   * later). If Android still wants them to confirm, its prompt is shown
   * when [interactive] (the person tapped Install), and the install is
   * dropped otherwise. Returns `started`, `permission` (HearthDrive must first be
   * allowed to install apps; the setting is opened) or `missing`.
   */
  private fun installUpdate(path: String, interactive: Boolean): String {
    val file = updatePackage(path) ?: return "missing"
    if (Build.VERSION.SDK_INT >= 26 && !packageManager.canRequestPackageInstalls()) {
      if (interactive) {
        runOnUiThread {
          startActivity(Intent(Settings.ACTION_MANAGE_UNKNOWN_APP_SOURCES, Uri.parse("package:$packageName")))
        }
      }
      return "permission"
    }
    val installer = packageManager.packageInstaller
    val params = PackageInstaller.SessionParams(PackageInstaller.SessionParams.MODE_FULL_INSTALL).apply {
      setAppPackageName(packageName)
      if (Build.VERSION.SDK_INT >= 31) setRequireUserAction(PackageInstaller.SessionParams.USER_ACTION_NOT_REQUIRED)
    }
    val id = try {
      installer.createSession(params)
    } catch (e: Exception) {
      Log.w(TAG, "Could not start installing the update: $e")
      return "missing"
    }
    try {
      installer.openSession(id).use { session ->
        session.openWrite("package.apk", 0, file.length()).use { out ->
          file.inputStream().use { it.copyTo(out, 256 * 1024) }
          session.fsync(out)
        }
        val result = Intent(this, UpdateResultReceiver::class.java)
          .setPackage(packageName)
          .putExtra(UpdateResultReceiver.EXTRA_INTERACTIVE, interactive)
        // Mutable: the installer adds the outcome to it.
        val flags = PendingIntent.FLAG_UPDATE_CURRENT or (if (Build.VERSION.SDK_INT >= 31) PendingIntent.FLAG_MUTABLE else 0)
        val pending = PendingIntent.getBroadcast(this, if (interactive) 1 else 2, result, flags)
        session.commit(pending.intentSender)
      }
    } catch (e: Exception) {
      Log.w(TAG, "Could not install the update: $e")
      try { installer.abandonSession(id) } catch (_: Exception) {}
      return "missing"
    }
    return "started"
  }

  // -- Share menu ----------------------------------------------------------

  private fun receiveShare(intent: Intent?) {
    if (intent == null) return
    val action = intent.action
    if (action != Intent.ACTION_SEND && action != Intent.ACTION_SEND_MULTIPLE) return
    val uris = mutableListOf<Uri>()
    if (action == Intent.ACTION_SEND) {
      streamExtra(intent)?.let { uris.add(it) }
    } else {
      streamListExtra(intent)?.let { uris.addAll(it) }
    }
    val text = if (uris.isEmpty()) intent.getStringExtra(Intent.EXTRA_TEXT) else null
    val subject = intent.getStringExtra(Intent.EXTRA_SUBJECT)
    // Handle each share once.
    intent.action = null
    val inbox = File(cacheDir, "shared").apply { mkdirs() }
    Thread {
      for (uri in uris) {
        try {
          copyIn(uri, inbox)
        } catch (e: Exception) {
          Logger.warn("Could not receive a shared file: $e")
        }
      }
      if (!text.isNullOrBlank()) {
        try {
          val name = uniqueName(inbox, safeName((subject?.take(60) ?: "Shared text") + ".txt"))
          File(inbox, name).writeText(text)
        } catch (e: Exception) {
          Logger.warn("Could not receive shared text: $e")
        }
      }
      notifyPage("nexus-shared")
    }.start()
  }

  @Suppress("DEPRECATION")
  private fun streamExtra(intent: Intent): Uri? =
    if (Build.VERSION.SDK_INT >= 33) intent.getParcelableExtra(Intent.EXTRA_STREAM, Uri::class.java)
    else intent.getParcelableExtra(Intent.EXTRA_STREAM)

  @Suppress("DEPRECATION")
  private fun streamListExtra(intent: Intent): List<Uri>? =
    if (Build.VERSION.SDK_INT >= 33) intent.getParcelableArrayListExtra(Intent.EXTRA_STREAM, Uri::class.java)
    else intent.getParcelableArrayListExtra(Intent.EXTRA_STREAM)

  /**
   * Copies one shared item into the inbox. Only content from other apps is
   * accepted: never `file://` paths or this app's own files, which another
   * app could otherwise trick HearthDrive into sending.
   */
  private fun copyIn(uri: Uri, inbox: File) {
    if (uri.scheme != ContentResolver.SCHEME_CONTENT) return
    if (uri.authority == "$packageName.fileprovider") return
    var name: String? = null
    contentResolver.query(uri, arrayOf(OpenableColumns.DISPLAY_NAME), null, null, null)?.use { c ->
      if (c.moveToFirst()) name = c.getString(0)
    }
    val finalName = uniqueName(inbox, safeName(name ?: uri.lastPathSegment ?: "Shared file"))
    val partial = File(inbox, "$finalName.part")
    contentResolver.openInputStream(uri)?.use { input ->
      partial.outputStream().use { output -> input.copyTo(output, 256 * 1024) }
    } ?: return
    partial.renameTo(File(inbox, finalName))
  }

  /** A plain file name: no folders, control characters or leading dots. */
  private fun safeName(name: String): String {
    val cleaned = name
      .map { c -> if (c.isISOControl() || c in "/\\:*?\"<>|") '_' else c }
      .joinToString("")
      .trim()
      .trimStart('.')
      .take(180)
    return cleaned.ifEmpty { "Shared file" }
  }

  /** `name.ext`, or `name (2).ext` and so on if that is taken. */
  private fun uniqueName(dir: File, name: String): String {
    val dot = name.lastIndexOf('.')
    val (stem, ext) = if (dot > 0) name.substring(0, dot) to name.substring(dot) else name to ""
    var candidate = name
    var n = 2
    while (File(dir, candidate).exists() || File(dir, "$candidate.part").exists()) {
      candidate = "$stem ($n)$ext"
      n++
    }
    return candidate
  }
}

private const val TAG = "NexusUpdate"

/**
 * Hears how an update install went. If Android wants the person to
 * confirm, its prompt is shown when they tapped Install, and the install
 * is dropped when it was started in the background (it is tried again
 * next time, or the person taps Install).
 */
class UpdateResultReceiver : BroadcastReceiver() {
  companion object {
    const val EXTRA_INTERACTIVE = "app.nexuscloud.desktop.INTERACTIVE"
  }

  override fun onReceive(context: Context, intent: Intent) {
    val status = intent.getIntExtra(PackageInstaller.EXTRA_STATUS, PackageInstaller.STATUS_FAILURE)
    when (status) {
      PackageInstaller.STATUS_PENDING_USER_ACTION -> {
        if (intent.getBooleanExtra(EXTRA_INTERACTIVE, false)) {
          @Suppress("DEPRECATION")
          val confirm: Intent? =
            if (Build.VERSION.SDK_INT >= 33) intent.getParcelableExtra(Intent.EXTRA_INTENT, Intent::class.java)
            else intent.getParcelableExtra(Intent.EXTRA_INTENT)
          confirm?.addFlags(Intent.FLAG_ACTIVITY_NEW_TASK)?.let { context.startActivity(it) }
        } else {
          val id = intent.getIntExtra(PackageInstaller.EXTRA_SESSION_ID, -1)
          if (id >= 0) try { context.packageManager.packageInstaller.abandonSession(id) } catch (_: Exception) {}
        }
      }
      PackageInstaller.STATUS_SUCCESS -> Log.i(TAG, "Update installed")
      else -> Log.w(TAG, "Update not installed ($status): ${intent.getStringExtra(PackageInstaller.EXTRA_STATUS_MESSAGE)}")
    }
  }
}
