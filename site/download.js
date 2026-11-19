// Fills in the download links from the latest GitHub release. Without
// JavaScript, or when GitHub doesn't answer, every link still goes to the
// latest release's page.
(function () {
  var REPO = 'Mohdjey123/hearthdrive';
  var android = /Android/i.test(navigator.userAgent);
  var $ = function (id) { return document.getElementById(id); };

  if (android) {
    $('hero-button').textContent = 'Download for Android';
    $('hero-other').textContent = 'Windows installer and other downloads';
  }

  function size(bytes) {
    return (bytes / 1048576).toFixed(bytes < 10485760 ? 1 : 0) + ' MB';
  }

  function day(iso) {
    return new Date(iso).toLocaleDateString('en-GB', { day: 'numeric', month: 'long', year: 'numeric' });
  }

  fetch('https://api.github.com/repos/' + REPO + '/releases/latest', { headers: { Accept: 'application/vnd.github+json' } })
    .then(function (r) { if (!r.ok) throw new Error(r.status); return r.json(); })
    .then(function (rel) {
      var assets = rel.assets || [];
      var win = assets.filter(function (a) { return /_x64-setup\.exe$/.test(a.name); })[0];
      var apk = assets.filter(function (a) { return /\.apk$/.test(a.name); })[0];
      var version = String(rel.tag_name || '').replace(/^v/, '');

      if (win) {
        $('win-link').href = win.browser_download_url;
        $('win-name').textContent = win.name + ' · ' + size(win.size);
      }
      if (apk) {
        $('apk-link').href = apk.browser_download_url;
        $('apk-name').textContent = apk.name + ' · ' + size(apk.size);
      }

      var mine = android ? apk : win;
      if (mine) {
        $('hero-button').href = mine.browser_download_url;
        $('hero-meta').textContent = 'Version ' + version + ', ' + size(mine.size) + '. Free. ' +
          (android ? 'For Android phones.' : 'For Windows 10 and 11.');
      }

      var line = $('release-line');
      line.textContent = 'Version ' + version + ', released ' + day(rel.published_at) + '. ';
      var notes = document.createElement('a');
      notes.href = rel.html_url;
      notes.textContent = 'What changed';
      line.appendChild(notes);
    })
    .catch(function () { /* the static links already point at the releases page */ });
})();
