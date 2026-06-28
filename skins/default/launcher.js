/* ============================================================
   天堂 3.8 登入器 — Default Skin / IPC Bridge
   ============================================================
   雙向溝通：
     Rust → JS：呼叫 window.lineage.setServers(...) / setVersion(...)
     JS → Rust：window.chrome.webview.postMessage({ type, ... })
   兩個 view：選擇伺服器、更新進度（Press Start）
   ============================================================ */

(function () {
  'use strict';

  let servers = [];
  let selectedIdx = 0;
  let windowed = true;
  // WindowMode: 4=400x300, 5=800x600, 6=1200x900, 7=1600x1200。預設 5(最安全)。
  let windowMode = 5;
  let currentView = 'progress'; // 'progress' (預設首頁 = Press Start) | 'select'
  // UI 是否被鎖定：自動更新進行中時禁止啟動遊戲、禁止進入伺服器選擇頁
  let locked = false;
  // 每個伺服器的探測狀態 {idx: 'checking'|'online'|'offline'}，渲染時沿用，避免重建掉資料
  const serverStatus = {};
  // 上方 tab 列「官網/客服」連結 URL；空字串會隱藏對應 span
  const topbarLinks = { official: '', support: '' };

  /** 切換 view */
  function setView(name) {
    currentView = name;
    const sel = document.getElementById('viewSelect');
    const prog = document.getElementById('viewProgress');
    if (sel) sel.classList.toggle('hidden', name !== 'select');
    if (prog) prog.classList.toggle('hidden', name !== 'progress');
  }

  /** 把伺服器陣列渲染到清單。
   *  IP/port 不再顯示（資訊太多干擾）；狀態燈號實際由 lineage.setServerStatus(idx,online)
   *  從 Rust 端 TCP 探測結果更新。 */
  function renderServers() {
    const root = document.getElementById('serverList');
    if (!root) return;
    root.innerHTML = '';
    servers.forEach((s, i) => {
      const row = document.createElement('div');
      row.className = 'server-row';
      row.dataset.idx = String(i);
      if (i === selectedIdx) row.classList.add('selected');
      if (!s.used) row.classList.add('offline');

      const dot = document.createElement('span');
      // 沿用先前探測結果（若還沒探測完則維持 checking）
      const st = serverStatus[i] || 'checking';
      dot.className = `status-dot ${st}`;

      const name = document.createElement('span');
      name.className = 'server-name';
      name.textContent = s.name || `Server ${i + 1}`;

      row.appendChild(dot);
      row.appendChild(name);

      // 點擊只切換選取狀態（不重建整列），避免燈號被重設成 checking
      row.addEventListener('click', () => {
        selectedIdx = i;
        root.querySelectorAll('.server-row.selected').forEach(r => r.classList.remove('selected'));
        row.classList.add('selected');
        sendToRust({ type: 'select', index: i });
      });
      row.addEventListener('dblclick', () => {
        if (locked) return;
        doLaunch();
      });

      root.appendChild(row);
    });
  }

  /** 視窗化 checkbox 切換 */
  function setWindowed(v) {
    windowed = !!v;
    const box = document.getElementById('cbWindowedBox');
    if (box) {
      box.textContent = windowed ? '✓' : '';
      box.classList.toggle('unchecked', !windowed);
    }
    // 全螢幕時解析度其實由 dgVoodoo / 顯示器決定,下拉鎖起來避免使用者誤會
    const sel = document.getElementById('resolutionSelect');
    if (sel) sel.disabled = !windowed;
  }

  /** 解析度下拉切換 */
  function setWindowMode(v) {
    const n = parseInt(v, 10);
    if ([4, 5, 6, 7].includes(n)) windowMode = n;
    const sel = document.getElementById('resolutionSelect');
    if (sel) sel.value = String(windowMode);
  }

  /** 觸發啟動 */
  function doLaunch() {
    if (locked) {
      setStatus('UI 鎖定中，無法啟動');
      return;
    }
    if (selectedIdx < 0 || selectedIdx >= servers.length) {
      setStatus('請先點選一個伺服器再按登入');
      return;
    }
    const s = servers[selectedIdx];
    setStatus(`正在啟動遊戲：${s.name || ('Server ' + selectedIdx)} (${s.ip}:${s.port})`);
    sendToRust({ type: 'launch', serverIdx: selectedIdx, windowed, windowMode });
  }

  /** 鎖定/解鎖 UI（自動更新進行中：true，更新失敗/取消/略過：false） */
  function setLocked(v) {
    locked = !!v;
    document.body.classList.toggle('ui-locked', locked);
  }

  // 紀錄 .press-start 原文（首次呼叫時抓），這樣空字串可以還原
  let pressStartOriginal = null;

  /** 顯示狀態文字：接管 .press-start 那條（與 Press Start 共用同一行）。
   *  text 空字串會還原為「Press Start' to play The Game.」 */
  function setStatus(text) {
    const ps = document.querySelector('.press-start');
    if (!ps) return;
    if (pressStartOriginal === null) pressStartOriginal = ps.textContent;
    const t = (text || '').toString().trim();
    ps.textContent = t.length > 0 ? t : pressStartOriginal;
    ps.classList.toggle('status-active', t.length > 0);
  }

  /** 統一 send：相容 Edge WebView2 與一般 webview */
  function sendToRust(payload) {
    try {
      if (window.chrome && window.chrome.webview) {
        window.chrome.webview.postMessage(JSON.stringify(payload));
        return;
      }
      if (window.ipc && typeof window.ipc.postMessage === 'function') {
        window.ipc.postMessage(JSON.stringify(payload));
        return;
      }
    } catch (e) {
      console.error('postMessage 失敗', e);
    }
  }

  // === 對外 API（Rust 端注入時呼叫）===
  window.lineage = {
    setServers(list) {
      servers = Array.isArray(list) ? list : [];
      selectedIdx = Math.min(selectedIdx, Math.max(0, servers.length - 1));
      // 換新清單時清掉舊的探測快取（會重新觸發 TCP 探測）
      Object.keys(serverStatus).forEach(k => delete serverStatus[k]);
      renderServers();
    },
    setVersion(text) {
      const el = document.getElementById('versionLabel');
      if (el) el.textContent = text;
    },
    setSelected(idx) {
      if (idx >= 0 && idx < servers.length) {
        selectedIdx = idx;
        renderServers();
      }
    },
    /** 設定進度條：current 與 total 各為 0~100，超出範圍會被 clamp */
    setProgress(current, total) {
      const clamp = (v) => Math.max(0, Math.min(100, Number(v) || 0));
      const cur = document.getElementById('progressCurrent');
      const tot = document.getElementById('progressTotal');
      if (cur) cur.style.width = clamp(current) + '%';
      if (tot) tot.style.width = clamp(total) + '%';
    },
    /** 設定上方 tab 連結 URL。{official: 'http://...', support: 'http://...'} */
    setLinks(map) {
      if (!map) return;
      topbarLinks.official = (map.official || '').trim();
      topbarLinks.support = (map.support || '').trim();
      const off = document.getElementById('topbarOfficial');
      const sup = document.getElementById('topbarSupport');
      if (off) off.style.display = topbarLinks.official ? '' : 'none';
      if (sup) sup.style.display = topbarLinks.support ? '' : 'none';
    },
    /** 設定公告 iframe URL；空字串或 null 則隱藏 iframe */
    setAnnouncement(url) {
      const frame = document.getElementById('announcementFrame');
      if (!frame) return;
      const trimmed = (url || '').trim();
      if (trimmed) {
        frame.src = trimmed;
        frame.classList.add('visible');
      } else {
        frame.removeAttribute('src');
        frame.classList.remove('visible');
      }
    },
    /** 由 Rust 端載入 launcher.ini 後 push 過來的玩家偏好。
     *  {windowed: bool, windowMode: 4..=7} — 任一缺欄會走預設(true / 5)。 */
    setPrefs(p) {
      if (!p) return;
      if (typeof p.windowed === 'boolean') setWindowed(p.windowed);
      if (typeof p.windowMode === 'number') setWindowMode(p.windowMode);
    },
    setView,
    setLocked,
    setStatus,
    /** 由 Rust 端 TCP 探測完成後呼叫，更新該伺服器列的狀態燈號。
     *  online=true 顯示綠燈、false 顯示紅燈，移除「探測中」狀態。
     *  狀態同步寫入 serverStatus 快取，後續任何 renderServers 都會沿用。 */
    setServerStatus(idx, online) {
      const status = online ? 'online' : 'offline';
      serverStatus[idx] = status;
      const root = document.getElementById('serverList');
      if (!root) return;
      const row = root.querySelector(`.server-row[data-idx="${idx}"]`);
      if (!row) return;
      const dot = row.querySelector('.status-dot');
      if (!dot) return;
      dot.classList.remove('checking', 'online', 'offline');
      dot.classList.add(status);
      // 列本身的 .offline class（影響整列灰化）也跟著實際狀態走
      row.classList.toggle('offline', !online);
    },
    showError(msg) { window.alert(msg); },
  };

  /** 把當前 windowed + windowMode push 給 Rust 端寫入 launcher.ini */
  function persistPrefs() {
    sendToRust({ type: 'savePrefs', windowed, windowMode });
  }

  // === 全域點擊代理 ===
  document.addEventListener('click', (ev) => {
    // 視窗化 checkbox
    if (ev.target.closest('.checkbox-windowed')) {
      setWindowed(!windowed);
      persistPrefs();
      return;
    }
    const target = ev.target.closest('[data-action]');
    if (!target) return;
    const action = target.dataset.action;
    switch (action) {
      case 'goto-select':
        // Press Start 頁的「開始」：切到伺服器選擇頁（更新中禁止）
        if (locked) return;
        setView('select');
        break;
      case 'launch':
        // 伺服器選擇頁的「登入」：以目前選中的伺服器啟動遊戲（更新中禁止）
        if (locked) return;
        doLaunch();
        break;
      case 'back':
        // 伺服器選擇頁的「取消」：返回 Press Start 頁
        setView('progress');
        break;
      case 'cancel':
        sendToRust({ type: 'cancel' });
        break;
      case 'open-link': {
        // 上方 tab 列「官網」/「客服」：用系統預設瀏覽器開啟
        const key = target.dataset.key;
        const url = key && topbarLinks[key];
        if (url) sendToRust({ type: 'openurl', url });
        break;
      }
    }
  });

  // === 拖曳：標 .drag-region 的元素 mousedown 觸發 drag_window
  //     舊版 WebView2 不支援 app-region: drag CSS，需 JS 備援。 ===
  document.addEventListener('mousedown', (ev) => {
    if (ev.button !== 0) return; // 只處理左鍵
    let node = ev.target;
    while (node && node !== document.body) {
      if (node.classList && node.classList.contains('no-drag')) return;
      if (node.classList && node.classList.contains('drag-region')) {
        sendToRust({ type: 'drag' });
        return;
      }
      node = node.parentElement;
    }
  });

  // === 啟動：通知 Rust 我準備好接收伺服器資料 ===
  window.addEventListener('DOMContentLoaded', () => {
    setView('progress'); // 預設顯示 Press Start 頁
    setWindowed(true);
    setWindowMode(5);    // 預設 800x600;Rust 端 setPrefs 會在 ready 後覆蓋成上次選擇
    const sel = document.getElementById('resolutionSelect');
    if (sel) {
      sel.addEventListener('change', () => {
        setWindowMode(sel.value);
        persistPrefs();
      });
    }
    sendToRust({ type: 'ready' });
  });
})();
