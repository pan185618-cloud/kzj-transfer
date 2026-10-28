// 跨机中转 UI — Tauri v2 global API (withGlobalTauri).
const { invoke } = window.__TAURI__.core;
const { listen } = window.__TAURI__.event;
const { open, save } = window.__TAURI__.dialog;
const { getCurrentWindow, LogicalSize, PhysicalPosition, currentMonitor } = window.__TAURI__.window;

// Android WebView: no window management, no file-path drag/drop, no Finder-style
// clipboard paths. Detect it once and hide the controls that cannot work there.
const IS_ANDROID = /android/i.test(navigator.userAgent);
if (IS_ANDROID) document.body.classList.add("android");

// Android WebView 键盘弹出时视口高度会缩小:给 body 加 kb-open,
// CSS 里把分类栏临时收起,给输入框腾位置。
if (IS_ANDROID && window.visualViewport) {
  const vv = window.visualViewport;
  const check = () => {
    const ratio = vv.height / window.screen.height;
    document.body.classList.toggle("kb-open", ratio < 0.65);
    // 键盘盖住的高度:底部抽屉/菜单要整体抬高,否则输入框被挡住
    const covered = Math.max(0, window.innerHeight - vv.height - vv.offsetTop);
    document.documentElement.style.setProperty("--kb", `${covered}px`);
  };
  vv.addEventListener("resize", check);
  vv.addEventListener("scroll", check);
}

// 发现设备面板里的输入框获得焦点时,面板顶到屏幕上方,避免被软键盘挡住
// (edge-to-edge 下 WebView 不随键盘缩小,visualViewport 也不一定变化)
document.addEventListener("focusin", (e) => {
  if (IS_ANDROID && e.target.closest && e.target.closest(".sheet")) document.body.classList.add("sheet-typing");
});
document.addEventListener("focusout", () => document.body.classList.remove("sheet-typing"));

const $ = (id) => document.getElementById(id);
const el = (tag, cls, text) => {
  const n = document.createElement(tag);
  if (cls) n.className = cls;
  if (text != null) n.textContent = text;
  return n;
};

const COMPACT_W = 320;
const COMPACT_H = 36;

let devices = [];
// 「按设备」视图当前查看的设备
let selectedFingerprint = null;
// 发送目标:默认所有在线设备都收;用户在「发现设备」里取消勾选的记在 excluded 里
const excluded = new Set(JSON.parse(localStorage.getItem("excludedTargets") || "[]"));
const saveExcluded = () => localStorage.setItem("excludedTargets", JSON.stringify([...excluded]));
const targetDevices = () => devices.filter((d) => !excluded.has(d.fingerprint));
let pendingRequest = null;
let islandText = "中转";
// 消息历史: fingerprint -> [{dir,kind,text,files,alias,ts,state,ratio,statusText}]
const history = new Map();
// 视图: all 全部 / device 按设备 / type 按类型
let view = "all";
let typeFilter = "image"; // image / file / link / date
let pendingPaths = null; // 选好文件但还没选设备时暂存
let scanning = false;

function fmtBytes(n) {
  if (n < 1024) return `${n} B`;
  if (n < 1024 * 1024) return `${(n / 1024).toFixed(1)} KB`;
  if (n < 1024 * 1024 * 1024) return `${(n / 1024 / 1024).toFixed(1)} MB`;
  return `${(n / 1024 / 1024 / 1024).toFixed(2)} GB`;
}
const fmtSpeed = (bps) => (bps > 0 ? `${fmtBytes(Math.round(bps))}/s` : "");
const pad2 = (n) => String(n).padStart(2, "0");
const fmtTime = (ts) => {
  const d = new Date(ts);
  return `${pad2(d.getHours())}:${pad2(d.getMinutes())}`;
};
function dayKey(ts) {
  const d = new Date(ts);
  return `${d.getFullYear()}-${pad2(d.getMonth() + 1)}-${pad2(d.getDate())}`;
}
function dayLabel(ts) {
  const today = new Date();
  const k = dayKey(ts);
  if (k === dayKey(today)) return "今天";
  const y = new Date(today);
  y.setDate(today.getDate() - 1);
  if (k === dayKey(y)) return "昨天";
  const d = new Date(ts);
  const wk = "日一二三四五六"[d.getDay()];
  return d.getFullYear() === today.getFullYear()
    ? `${d.getMonth() + 1}月${d.getDate()}日 周${wk}`
    : `${d.getFullYear()}年${d.getMonth() + 1}月${d.getDate()}日`;
}

// ---------- 提示条(替代原来堆在底部的日志) ----------

// 后端的启动/发现类日志对用户没有意义,只在控制台留底
const QUIET = new Set(["system"]);

function logLine(category, text) {
  console.log(`[${category}] ${text}`);
  if (QUIET.has(category)) return;
  const kind = category === "error" || /失败|错误|无法|不可用/.test(text) ? "err" : "info";
  toast(text, kind);
}

function toast(text, kind = "info") {
  const box = $("toasts");
  const t = el("div", `toast ${kind}`, text);
  box.appendChild(t);
  while (box.children.length > 3) box.firstChild.remove();
  setTimeout(() => {
    t.classList.add("out");
    setTimeout(() => t.remove(), 300);
  }, kind === "err" ? 5000 : 2600);
}

// ---------- 设备 ----------

function deviceIcon(d) {
  const t = (d && d.deviceType || "").toLowerCase();
  if (t.includes("mobile")) {
    return '<svg viewBox="0 0 24 24"><rect x="7" y="3" width="10" height="18" rx="2.2" fill="none" stroke="currentColor" stroke-width="1.8"/><path d="M11 18h2" stroke="currentColor" stroke-width="1.8" stroke-linecap="round"/></svg>';
  }
  return '<svg viewBox="0 0 24 24"><rect x="3.5" y="5" width="17" height="11" rx="1.8" fill="none" stroke="currentColor" stroke-width="1.8"/><path d="M2 19h20" stroke="currentColor" stroke-width="1.8" stroke-linecap="round"/></svg>';
}

function deviceByFp(fp) {
  return devices.find((d) => d.fingerprint === fp);
}

// 历史里出现过的设备(可能已离线)也要能在「按设备」里看到
function knownDevices() {
  const list = devices.map((d) => ({ fp: d.fingerprint, alias: d.alias, online: true, d }));
  for (const [fp, msgs] of history) {
    if (!msgs.length || list.some((x) => x.fp === fp)) continue;
    list.push({ fp, alias: msgs[msgs.length - 1].alias || "未知设备", online: false, d: null });
  }
  return list;
}

function renderDevices() {
  const n = devices.length;
  $("discover-count").textContent = String(n);
  $("discover-count").classList.toggle("hidden", n === 0);
  $("brand-sub").textContent = n ? `${n} 台设备在线` : scanning ? "正在搜索设备…" : "未发现设备";
  $("brand-sub").classList.toggle("on", n > 0);

  // 发送目标
  const ts = targetDevices();
  $("target").classList.toggle("hidden", n === 0);
  $("target-name").textContent =
    ts.length === 0 ? "未选设备"
    : ts.length === n && n > 1 ? `全部 ${n} 台设备`
    : ts.map((d) => d.alias).join("、");

  renderDevList();
  renderSubDevice();
  renderMessages();
}

// 「按设备」视图切换查看的设备
function selectDevice(fp) {
  selectedFingerprint = fp;
  renderDevices();
}

// 发现设备面板里勾选/取消一台设备作为发送目标
function toggleTarget(fp) {
  if (excluded.has(fp)) excluded.delete(fp);
  else excluded.add(fp);
  saveExcluded();
  renderDevices();
  flushPending();
}

// 文件已选好、在等设备:有目标后自动发出
function flushPending() {
  if (pendingPaths && targetDevices().length) {
    const { paths, picked } = pendingPaths;
    pendingPaths = null;
    sendPaths(paths, picked);
  }
}

// 当前发送目标(可多台);一台都没有时打开「发现设备」
function ensureTarget() {
  if (targetDevices().length) return true;
  openSheet();
  toast(devices.length ? "先勾选要发送的设备" : "还没有发现设备");
  return false;
}

// ---------- 发现设备面板(自动搜索 + 手动连接) ----------

function renderDevList() {
  const list = $("dev-list");
  list.innerHTML = "";
  $("dev-empty").classList.toggle("hidden", devices.length > 0);
  for (const d of devices) {
    const li = el("li", "dev-item" + (!excluded.has(d.fingerprint) ? " selected" : ""));
    const icon = el("span", "dev-icon");
    icon.innerHTML = deviceIcon(d);
    const info = el("span", "dev-info");
    info.append(el("b", null, d.alias), el("small", null, `${d.host}${d.paired ? " · 已信任" : ""}`));
    const check = el("span", "dev-check");
    check.innerHTML = '<svg viewBox="0 0 24 24"><path d="m5 12.5 4.5 4.5L19 7.5" fill="none" stroke="currentColor" stroke-width="2.4" stroke-linecap="round" stroke-linejoin="round"/></svg>';
    li.append(icon, info, check);
    li.onclick = () => toggleTarget(d.fingerprint);
    list.appendChild(li);
  }
}

function openSheet() {
  $("discover-sheet").classList.remove("hidden");
  renderDevList();
  scan(true);
}
function closeSheet() {
  $("discover-sheet").classList.add("hidden");
}

async function scan(quiet = false) {
  if (scanning) return;
  scanning = true;
  $("discover-sheet").classList.add("scanning");
  renderDevices();
  try {
    await invoke("refresh_devices");
  } catch (err) {
    if (!quiet) logLine("error", `搜索失败: ${err}`);
  } finally {
    // 发现是异步回报的,留一点时间给回包
    setTimeout(() => {
      scanning = false;
      $("discover-sheet").classList.remove("scanning");
      renderDevices();
    }, 1500);
  }
}

$("btn-discover").onclick = openSheet;
$("hint-discover").onclick = openSheet;
$("target-change").onclick = openSheet;
$("sheet-close").onclick = closeSheet;
document.querySelector(".sheet-backdrop").onclick = closeSheet;
$("btn-scan").onclick = () => scan(false);

$("btn-add-ip").onclick = async () => {
  const ip = $("ip-input").value.trim();
  if (!ip) return;
  const btn = $("btn-add-ip");
  btn.disabled = true;
  btn.textContent = "连接中…";
  try {
    await invoke("add_device_ip", { ip, port: null });
    $("ip-input").value = "";
    toast(`已连接 ${ip}`);
  } catch (err) {
    logLine("error", `连接 ${ip} 失败: ${err}`);
  } finally {
    btn.disabled = false;
    btn.textContent = "连接";
  }
};
$("ip-input").addEventListener("keydown", (e) => {
  if (e.key === "Enter") $("btn-add-ip").click();
});

// ---------- 分类:全部 / 按设备 / 按类型 ----------

function setView(v) {
  view = v;
  document.querySelectorAll(".seg").forEach((b) => b.classList.toggle("active", b.dataset.view === v));
  const idx = ["all", "device", "type"].indexOf(v);
  document.querySelector(".segmented").style.setProperty("--i", idx);
  $("sub-device").classList.toggle("hidden", v !== "device");
  $("sub-type").classList.toggle("hidden", v !== "type");
  if (v === "device" && !selectedFingerprint) {
    const k = knownDevices();
    if (k.length) selectedFingerprint = k[0].fp;
  }
  renderDevices();
}
document.querySelectorAll(".seg").forEach((b) => (b.onclick = () => setView(b.dataset.view)));
document.querySelectorAll("#sub-type .pill").forEach((b) => {
  b.onclick = () => {
    typeFilter = b.dataset.type;
    document.querySelectorAll("#sub-type .pill").forEach((x) => x.classList.toggle("active", x === b));
    renderMessages();
  };
});

function renderSubDevice() {
  const bar = $("sub-device");
  bar.innerHTML = "";
  const list = knownDevices();
  if (!list.length) {
    bar.appendChild(el("span", "sub-empty", "还没有设备,点右上角「发现设备」"));
    return;
  }
  for (const k of list) {
    const b = el("button", "pill dev-pill" + (k.fp === selectedFingerprint ? " active" : "") + (k.online ? "" : " offline"));
    const icon = el("span", "pill-icon");
    icon.innerHTML = deviceIcon(k.d);
    b.append(icon, document.createTextNode(k.alias));
    const n = (history.get(k.fp) || []).length;
    if (n) b.appendChild(el("span", "pill-count", String(n)));
    b.onclick = () => selectDevice(k.fp);
    bar.appendChild(b);
  }
}

// ---------- 消息区渲染 ----------

const URL_RE = /https?:\/\/[^\s"'<>]+/g;

function openLink(url) {
  invoke("open_link", { url }).catch((err) => logLine("error", `${err}`));
}

function linkify(text) {
  const frag = document.createDocumentFragment();
  let last = 0;
  for (const m of text.matchAll(URL_RE)) {
    if (m.index > last) frag.appendChild(document.createTextNode(text.slice(last, m.index)));
    const a = el("a", null, m[0]);
    a.href = "#";
    a.onclick = (e) => {
      e.preventDefault();
      openLink(m[0]);
    };
    makeDraggable(a, () => ({ url: m[0] }));
    frag.appendChild(a);
    last = m.index + m[0].length;
  }
  if (last < text.length) frag.appendChild(document.createTextNode(text.slice(last)));
  return frag;
}

const mimeOf = (f) => (f.fileType || f.file_type || "").toLowerCase();
const isImage = (f) => mimeOf(f).startsWith("image/");

// 文件类型 → [标签, 颜色]
function fileKind(f) {
  const t = mimeOf(f);
  const ext = ((f.name || "").split(".").pop() || "").toLowerCase();
  if (t.startsWith("image/")) return ["IMG", "c-green"];
  if (t.startsWith("video/")) return ["VID", "c-purple"];
  if (t.startsWith("audio/")) return ["AUD", "c-pink"];
  if (t.includes("pdf")) return ["PDF", "c-red"];
  if (t.includes("zip") || t.includes("compressed") || t.includes("tar") || ["rar", "7z", "gz"].includes(ext)) return ["ZIP", "c-amber"];
  if (t.includes("word") || ["doc", "docx"].includes(ext)) return ["DOC", "c-blue"];
  if (t.includes("sheet") || t.includes("excel") || ["xls", "xlsx", "csv"].includes(ext)) return ["XLS", "c-green"];
  if (t.includes("presentation") || ["ppt", "pptx"].includes(ext)) return ["PPT", "c-orange"];
  if (t.startsWith("text/")) return ["TXT", "c-slate"];
  if (ext && ext !== (f.name || "").toLowerCase() && ext.length <= 4) return [ext.toUpperCase(), "c-slate"];
  return ["FILE", "c-slate"];
}

function kindBadge(f) {
  const [label, color] = fileKind(f);
  return el("span", `f-badge ${color}`, label);
}

// ---------- 文件夹分组显示 ----------
// name 形如 "目录名/子路径/文件"(桌面 collect_path 与 Android 选文件夹都这样命名):
// 第一段是顶层目录名 → 该文件归入这个文件夹组;不带 "/" 的是顶层散文件。

function isFolderMsg(m) {
  return m.kind === "files" && (m.files || []).some((f) => (f.name || "").includes("/"));
}

function groupByFolder(files) {
  const groups = [];
  const byName = new Map();
  for (const f of files || []) {
    const seg = (f.name || "").split("/")[0] || f.name;
    if ((f.name || "").includes("/")) {
      if (!byName.has(seg)) {
        const g = { name: seg, files: [], size: 0, dirPath: null };
        byName.set(seg, g);
        groups.push(g);
      }
      const g = byName.get(seg);
      g.files.push(f);
      g.size += f.size || 0;
      if (!g.dirPath) {
        const p = f.savedPath || f.saved_path || "";
        const idx = p.indexOf("/" + seg + "/");
        if (idx >= 0) g.dirPath = p.slice(0, idx + seg.length + 1);
      }
    } else {
      groups.push({ name: null, files: [f], size: f.size || 0, dirPath: null });
    }
  }
  return groups;
}

const FOLDER_SVG = '<svg viewBox="0 0 24 24"><path d="M3.5 7.5A2.5 2.5 0 0 1 6 5h3.6l2 2.2H18a2.5 2.5 0 0 1 2.5 2.5V17a2.5 2.5 0 0 1-2.5 2.5H6A2.5 2.5 0 0 1 3.5 17V7.5Z" fill="currentColor" opacity=".9"/></svg>';

function bubbleForFolder(g, dir) {
  const wrap = el("div", "f-folder");
  const head = el("div", "f-card f-folder-head");
  const icon = el("span", "f-badge c-amber folder");
  icon.innerHTML = FOLDER_SVG;
  const info = el("div", "f-info");
  info.append(el("div", "f-name", g.name), el("div", "f-sub", `文件夹 · ${g.files.length} 个文件 · ${fmtBytes(g.size)}`));
  head.append(icon, info);
  if (g.dirPath) {
    head.classList.add("openable");
    head.title = g.dirPath;
    makeDraggable(head, () => ({ paths: [g.dirPath], icon: dragIcon(null, "DIR", KIND_HEX["c-amber"]) }));
    head.onclick = () =>
      invoke("open_file", { path: g.dirPath }).catch((e) => logLine("error", `打开失败: ${e}`));
  }
  wrap.appendChild(head);

  const list = el("div", "f-folder-items");
  const MAX = 6;
  const render = (files) => {
    list.innerHTML = "";
    for (const f of files) {
      const item = bubbleForFile({ ...f, name: f.name.slice(g.name.length + 1) }, dir, true);
      item.classList.add("f-subitem");
      list.appendChild(item);
    }
  };
  render(g.files.slice(0, MAX));
  if (g.files.length > MAX) {
    const more = el("button", "f-more", `展开其余 ${g.files.length - MAX} 个文件`);
    more.onclick = (e) => {
      e.stopPropagation();
      render(g.files);
      more.remove();
    };
    wrap.append(list, more);
  } else {
    wrap.appendChild(list);
  }
  return wrap;
}

function loadThumb(img, path, onFail) {
  invoke("read_image_preview", { path })
    .then((url) => (img.src = url))
    .catch(onFail);
}

// ---------- 拖出(桌面):把文件/文件夹/图片/链接拖到 Finder、微信、QQ 等 ----------
// 按住后移动超过阈值才发起原生拖拽,单纯点击仍然是打开。
// 拖拽期间窗口自己也会收到 drop,用 draggingOut 屏蔽,避免把文件又发出去。

let draggingOut = false;
let suppressClick = false;
listen("drag-out-end", () => setTimeout(() => (draggingOut = false), 300));

// 小尺寸拖拽预览图:有缩略图用缩略图,否则画一个类型色块
function dragIcon(imgEl, label, color) {
  const S = 64;
  const c = document.createElement("canvas");
  c.width = c.height = S;
  const g = c.getContext("2d");
  g.beginPath();
  g.roundRect(2, 2, S - 4, S - 4, 12);
  g.clip();
  if (imgEl && imgEl.complete && imgEl.naturalWidth) {
    const r = Math.max(S / imgEl.naturalWidth, S / imgEl.naturalHeight);
    const w = imgEl.naturalWidth * r, h = imgEl.naturalHeight * r;
    g.drawImage(imgEl, (S - w) / 2, (S - h) / 2, w, h);
  } else {
    g.fillStyle = color || "#64748b";
    g.fillRect(0, 0, S, S);
    g.fillStyle = "#fff";
    g.font = "800 15px -apple-system, system-ui, sans-serif";
    g.textAlign = "center";
    g.textBaseline = "middle";
    g.fillText(label || "FILE", S / 2, S / 2);
  }
  return c.toDataURL("image/png");
}

const KIND_HEX = {
  "c-blue": "#3b82f6", "c-green": "#10b981", "c-red": "#ef4444", "c-amber": "#f59e0b",
  "c-purple": "#8b5cf6", "c-pink": "#ec4899", "c-orange": "#f97316", "c-slate": "#64748b",
};

// node 上按下鼠标并拖动时发起拖出;getPayload 返回 { paths?, url?, icon? }
function makeDraggable(node, getPayload) {
  if (IS_ANDROID) return;
  node.addEventListener("dragstart", (e) => e.preventDefault());
  // 不让 WebKit 发起它自己的网页拖拽(图片/文字默认可拖):它被 dragstart 取消时
  // 会清空系统拖拽剪贴板,而我们的原生拖拽正用着它,Finder 收到空内容就拒绝放下。
  node.style.webkitUserDrag = "none";
  node.querySelectorAll("img").forEach((i) => (i.draggable = false));
  node.addEventListener("mousedown", (e) => {
    if (e.button !== 0 || e.target.closest("button:not(.g-cell)")) return;
    const x0 = e.clientX, y0 = e.clientY;
    const move = (ev) => {
      if (Math.abs(ev.clientX - x0) + Math.abs(ev.clientY - y0) < 6) return;
      cleanup();
      const p = getPayload();
      if (!p) return;
      draggingOut = true;
      suppressClick = true;
      setTimeout(() => (suppressClick = false), 400);
      invoke("start_drag_out", { paths: p.paths || [], url: p.url || null, icon: p.icon || null }).catch((err) => {
        draggingOut = false;
        logLine("error", `拖出失败: ${err}`);
      });
    };
    const cleanup = () => {
      window.removeEventListener("mousemove", move);
      window.removeEventListener("mouseup", cleanup);
    };
    window.addEventListener("mousemove", move);
    window.addEventListener("mouseup", cleanup);
  });
  // 拖完松手时浏览器可能补发一次 click,吞掉它,别把文件打开
  node.addEventListener("click", (e) => {
    if (suppressClick) {
      e.preventDefault();
      e.stopImmediatePropagation();
    }
  }, true);
}

function fileDragPayload(f, path, imgEl) {
  const [label, color] = fileKind(f);
  return () => ({ paths: [path], icon: dragIcon(imgEl, label, KIND_HEX[color]) });
}

function saveAs(f, path) {
  return async (e) => {
    e.stopPropagation();
    try {
      const dot = (f.name || "").lastIndexOf(".");
      const ext = dot > 0 ? f.name.slice(dot + 1) : null;
      // 扩展名合法才加过滤器,避免对话框报错
      const filters = /^[a-zA-Z0-9]{1,10}$/.test(ext || "")
        ? [{ name: `${ext.toUpperCase()} 文件`, extensions: [ext] }, { name: "所有文件", extensions: ["*"] }]
        : undefined;
      const dst = await save({ defaultPath: (f.name || "").split("/").pop(), filters });
      if (!dst) return;
      await invoke("copy_file_to", { src: path, dst });
      toast(`已另存为 ${dst}`);
    } catch (err) {
      logLine("error", `另存为失败: ${err}`);
    }
  };
}

function bubbleForFile(f, dir, compact = false) {
  const card = el("div", "f-card");
  const info = el("div", "f-info");
  info.append(el("div", "f-name", f.name), el("div", "f-sub", fmtBytes(f.size || 0)));
  const path = f.savedPath || f.saved_path;
  const badge = kindBadge(f);
  let thumbEl = null;

  // 图片:在气泡里直接显示缩略图,失败回落到类型图标
  if (isImage(f) && path && !compact) {
    card.classList.add("has-thumb");
    const thumb = el("img", "f-thumb");
    thumbEl = thumb;
    thumb.alt = f.name || "";
    loadThumb(thumb, path, () => {
      thumb.remove();
      card.classList.remove("has-thumb");
      card.prepend(badge);
    });
    card.append(thumb, info);
  } else {
    card.append(badge, info);
  }
  if (path) {
    card.classList.add("openable");
    card.title = path;
    card.onclick = () =>
      invoke("open_file", { path }).catch((e) => logLine("error", `打开失败: ${e}`));
    makeDraggable(card, fileDragPayload(f, path, thumbEl));
    if (!IS_ANDROID && !compact) {
      const saveBtn = el("button", "f-save-as", "另存为");
      saveBtn.title = "保存到其他位置";
      saveBtn.onclick = saveAs(f, path);
      card.appendChild(saveBtn);
    }
  }
  return card;
}

function metaText(m) {
  const who = m.dir === "in" ? m.alias : `发给 ${m.alias}`;
  return view === "device" ? fmtTime(m.ts) : `${who} · ${fmtTime(m.ts)}`;
}

function msgNode(m) {
  const li = el("li", `msg ${m.dir}`);
  li.appendChild(el("div", "m-meta", metaText(m)));
  const bubble = el("div", "bubble" + (m.kind === "text" ? " text" : " files"));

  if (m.kind === "text") {
    bubble.appendChild(linkify(m.text));
  } else if (isFolderMsg(m)) {
    for (const g of groupByFolder(m.files)) {
      bubble.appendChild(g.name === null ? bubbleForFile(g.files[0], m.dir) : bubbleForFolder(g, m.dir));
    }
  } else if ((m.files || []).length === 0) {
    bubble.appendChild(el("div", "f-pending", m.dir === "in" ? "正在接收…" : "正在准备…"));
  } else {
    for (const f of m.files) bubble.appendChild(bubbleForFile(f, m.dir));
  }

  if (m.state && m.state !== "done") {
    const bar = el("div", "bar");
    const fill = el("div");
    fill.style.width = `${Math.min(100, Math.round((m.ratio || 0) * 100))}%`;
    bar.appendChild(fill);
    bubble.appendChild(bar);
    const row = el("div", "t-row");
    row.appendChild(el("span", "t-status" + (m.state === "failed" || m.state === "cancelled" ? " bad" : ""), m.statusText || ""));
    if (m.cancellable) {
      const btn = el("button", "t-cancel", "取消");
      btn.onclick = m.onCancel || (() => {});
      row.appendChild(btn);
    }
    bubble.appendChild(row);
  } else if (m.statusText && m.kind !== "text") {
    bubble.appendChild(el("div", "t-row", "")).appendChild(el("span", "t-status", m.statusText));
  }

  li.appendChild(bubble);
  return li;
}

function allMsgs(filterFp = null) {
  const out = [];
  const batches = new Map();
  for (const [fp, msgs] of history) {
    if (filterFp && fp !== filterFp) continue;
    for (const m of msgs) {
      // 同一次群发到多台设备的记录,不按设备筛选时合并成一条
      if (!filterFp && m.batch && m.dir === "out") {
        if (!batches.has(m.batch)) {
          batches.set(m.batch, []);
          out.push({ _batch: m.batch, ts: m.ts });
        }
        batches.get(m.batch).push(m);
        continue;
      }
      out.push(m);
    }
  }
  return out
    .map((m) => (m._batch ? mergeBatch(batches.get(m._batch)) : m))
    .sort((a, b) => a.ts - b.ts);
}

const STATE_ZH = { sending: "发送中", done: "已送达", failed: "失败", cancelled: "已取消" };

function mergeBatch(parts) {
  if (parts.length === 1) return parts[0];
  const first = parts.reduce((a, b) => (a.ts <= b.ts ? a : b));
  const any = (st) => parts.some((p) => p.state === st);
  const all = (st) => parts.every((p) => p.state === st);
  const state = any("sending") ? "sending" : all("done") ? "done" : any("failed") ? "failed" : any("cancelled") ? "cancelled" : "done";
  const statusText = state === "done" ? "" : parts
    .map((p) => `${p.alias} ${p.state === "sending" ? Math.round((p.ratio || 0) * 100) + "%" : STATE_ZH[p.state] || ""}`)
    .join(" · ");
  const sending = parts.filter((p) => p.state === "sending" && p.onCancel);
  return {
    ...first,
    alias: parts.map((p) => p.alias).join("、"),
    files: parts.find((p) => (p.files || []).length)?.files || first.files,
    state,
    statusText,
    ratio: parts.reduce((s, p) => s + (p.state === "sending" ? p.ratio || 0 : 1), 0) / parts.length,
    cancellable: sending.length > 0,
    onCancel: () => sending.forEach((p) => p.onCancel()),
  };
}

function dayDivider(ts) {
  const li = el("li", "day-divider");
  li.appendChild(el("span", null, dayLabel(ts)));
  return li;
}

// 带日期分隔的聊天流
function renderChat(ul, msgs) {
  let last = null;
  for (const m of msgs) {
    const k = dayKey(m.ts);
    if (k !== last) {
      ul.appendChild(dayDivider(m.ts));
      last = k;
    }
    ul.appendChild(msgNode(m));
  }
}

function renderImages(ul, msgs) {
  const items = [];
  for (const m of msgs) {
    if (m.kind !== "files") continue;
    for (const f of m.files || []) if (isImage(f) && (f.savedPath || f.saved_path)) items.push({ m, f });
  }
  if (!items.length) return 0;
  const li = el("li", "gallery");
  for (const { m, f } of items.reverse()) {
    const path = f.savedPath || f.saved_path;
    const cell = el("button", "g-cell");
    cell.title = `${f.name} · ${m.alias} · ${fmtTime(m.ts)}`;
    const img = el("img");
    img.alt = f.name;
    loadThumb(img, path, () => {
      img.remove();
      cell.appendChild(kindBadge(f));
    });
    cell.appendChild(img);
    cell.appendChild(el("span", `g-dir ${m.dir}`, m.dir === "in" ? "收" : "发"));
    cell.onclick = () => invoke("open_file", { path }).catch((e) => logLine("error", `打开失败: ${e}`));
    makeDraggable(cell, fileDragPayload(f, path, img));
    li.appendChild(cell);
  }
  ul.appendChild(li);
  return items.length;
}

function renderFiles(ul, msgs) {
  const rows = [];
  for (const m of msgs) {
    if (m.kind !== "files") continue;
    for (const f of m.files || []) if (!isImage(f)) rows.push({ m, f });
  }
  if (!rows.length) return 0;
  const li = el("li", "list-card");
  for (const { m, f } of rows.reverse()) {
    const row = bubbleForFile(f, m.dir, true);
    row.classList.add("list-row");
    row.querySelector(".f-sub").textContent =
      `${fmtBytes(f.size || 0)} · ${m.dir === "in" ? "来自 " : "发给 "}${m.alias} · ${dayLabel(m.ts)} ${fmtTime(m.ts)}`;
    li.appendChild(row);
  }
  ul.appendChild(li);
  return rows.length;
}

function renderLinks(ul, msgs) {
  const rows = [];
  for (const m of msgs) {
    if (m.kind !== "text") continue;
    for (const u of m.text.match(URL_RE) || []) rows.push({ m, u });
  }
  if (!rows.length) return 0;
  const li = el("li", "list-card");
  for (const { m, u } of rows.reverse()) {
    const row = el("div", "f-card list-row openable");
    const badge = el("span", "f-badge c-blue link");
    badge.innerHTML = '<svg viewBox="0 0 24 24"><path d="M10 14a4 4 0 0 0 5.7 0l3-3a4 4 0 0 0-5.7-5.7l-1 1M14 10a4 4 0 0 0-5.7 0l-3 3a4 4 0 0 0 5.7 5.7l1-1" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round"/></svg>';
    const info = el("div", "f-info");
    let host = u;
    try {
      host = new URL(u).host;
    } catch (_) {}
    info.append(el("div", "f-name", host), el("div", "f-sub", u));
    row.append(badge, info);
    row.onclick = () => openLink(u);
    makeDraggable(row, () => ({ url: u }));
    li.appendChild(row);
  }
  ul.appendChild(li);
  return rows.length;
}

const TYPE_EMPTY = { image: "还没有收发过图片", file: "还没有收发过文件", link: "还没有收发过链接", date: "还没有内容" };

function renderMessages() {
  scheduleSave();
  const ul = $("msg-list");
  ul.innerHTML = "";
  let count = 0;
  if (view === "all") {
    const msgs = allMsgs();
    renderChat(ul, msgs);
    count = msgs.length;
  } else if (view === "device") {
    const msgs = selectedFingerprint ? allMsgs(selectedFingerprint) : [];
    renderChat(ul, msgs);
    count = msgs.length;
  } else {
    const msgs = allMsgs();
    if (typeFilter === "image") count = renderImages(ul, msgs);
    else if (typeFilter === "file") count = renderFiles(ul, msgs);
    else if (typeFilter === "link") count = renderLinks(ul, msgs);
    else {
      // 日期:按天倒序分组的时间线
      const byDay = new Map();
      for (const m of msgs) {
        const k = dayKey(m.ts);
        if (!byDay.has(k)) byDay.set(k, []);
        byDay.get(k).push(m);
      }
      for (const k of [...byDay.keys()].sort().reverse()) {
        const ms = byDay.get(k);
        const head = dayDivider(ms[0].ts);
        head.firstChild.textContent += ` · ${ms.length} 条`;
        ul.appendChild(head);
        for (const m of ms) ul.appendChild(msgNode(m));
      }
      count = msgs.length;
    }
  }

  const noDevices = devices.length === 0 && !hasMessages();
  $("device-hint").classList.toggle("hidden", !noDevices);
  $("empty-hint").classList.toggle("hidden", noDevices || count > 0);
  if (!noDevices && count === 0) {
    $("empty-sub").textContent =
      view === "type" ? TYPE_EMPTY[typeFilter]
      : view === "device" && selectedFingerprint ? `和 ${(deviceByFp(selectedFingerprint) || {}).alias || "这台设备"} 之间还没有内容`
      : IS_ANDROID ? "输入文字或链接点「发送」,点左下角文件夹图标发送文件或整个文件夹"
      : "输入文字或链接按 Enter 发送;点左下角文件夹图标发文件/文件夹,也可以直接把文件拖进窗口或 ⌘V 粘贴";
  }
  // 时间线类视图滚到底部,列表类视图停在顶部(最新在上)
  const stage = $("stage");
  stage.scrollTop = view === "type" && typeFilter !== "date" ? 0 : stage.scrollHeight;
}

function hasMessages() {
  for (const msgs of history.values()) if (msgs.length) return true;
  return false;
}

// ---------- 历史记录 ----------

// ---------- 持久化:重启后记录还在 ----------
// 只存已结束的消息(传输中的有回调,重启后也无法继续);每台设备最多留 500 条。
const KEEP_FIELDS = ["dir", "kind", "text", "files", "alias", "ts", "state", "statusText", "batch"];
const MAX_PER_DEVICE = 500;
let historyLoaded = false;
let saveTimer = null;

function serializeHistory() {
  const out = [];
  for (const [fp, msgs] of history) {
    const done = msgs.filter((m) => m.state !== "sending").slice(-MAX_PER_DEVICE);
    if (!done.length) continue;
    out.push({
      fp,
      msgs: done.map((m) => {
        const o = {};
        for (const k of KEEP_FIELDS) if (m[k] !== undefined) o[k] = m[k];
        return o;
      }),
    });
  }
  return JSON.stringify(out);
}

function scheduleSave() {
  if (!historyLoaded) return; // 还没读完旧记录前不写,免得把旧记录覆盖成空
  clearTimeout(saveTimer);
  saveTimer = setTimeout(() => {
    invoke("save_history", { json: serializeHistory() }).catch((e) => console.warn("save_history", e));
  }, 600);
}

async function loadHistory() {
  try {
    const list = JSON.parse((await invoke("load_history")) || "[]");
    for (const { fp, msgs } of list) {
      if (!fp || !Array.isArray(msgs)) continue;
      // 启动后才到的新消息(极少)排在旧记录之后
      const old = msgs.map((m) => ({ ...m, _old: true }));
      history.set(fp, [...old, ...(history.get(fp) || [])]);
    }
  } catch (e) {
    console.warn("load_history", e);
  }
  historyLoaded = true;
}

function pushMsg(fp, msg) {
  if (!history.has(fp)) history.set(fp, []);
  history.get(fp).push(msg);
  renderSubDevice();
  renderMessages();
}

// ---------- 发送 ----------

async function doSendText() {
  const text = $("composer-input").value.trim();
  if (!text) return;
  if (!ensureTarget()) return;
  const batch = newBatch();
  const results = await Promise.allSettled(
    targetDevices().map((d) =>
      invoke("send_text", { fingerprint: d.fingerprint, text, batch }).catch((err) => {
        logLine("error", `发给 ${d.alias} 失败: ${err}`);
        throw err;
      })
    )
  );
  if (results.some((r) => r.status === "fulfilled")) {
    $("composer-input").value = "";
    autosize();
  }
}

// 一次发送动作的编号:发给多台设备时,各设备的记录共用它,「全部」视图里合并成一个气泡
const newBatch = () => (crypto.randomUUID ? crypto.randomUUID() : `${Date.now()}-${Math.random()}`);

$("composer-send").onclick = doSendText;
$("composer-input").addEventListener("keydown", (e) => {
  // Android 软键盘的回车是换行,发送交给「发送」按钮
  if (!IS_ANDROID && e.key === "Enter" && !e.shiftKey && !e.isComposing) {
    e.preventDefault();
    doSendText();
  }
});

function autosize() {
  const ta = $("composer-input");
  ta.style.height = "auto";
  ta.style.height = Math.min(ta.scrollHeight, 120) + "px";
  $("composer").classList.toggle("has-text", ta.value.trim().length > 0);
}
$("composer-input").addEventListener("input", autosize);

// ---------- 粘贴 / 读取剪贴板 ----------
// 输入框里 ⌘V/Ctrl+V,或点「粘贴」按钮:剪贴板里是文件就发文件,是图片(截图、
// 网页图片)就存成 PNG 发出去,是文字就填进输入框。
// macOS 上 WKWebView 拿不到文件绝对路径和截图,走原生命令读 NSPasteboard;
// Windows/Android 用网页剪贴板里的图片字节。

function insertText(text) {
  const ta = $("composer-input");
  const s = ta.selectionStart ?? ta.value.length;
  const t = ta.selectionEnd ?? s;
  ta.value = ta.value.slice(0, s) + text + ta.value.slice(t);
  ta.selectionStart = ta.selectionEnd = s + text.length;
  ta.focus();
  setTimeout(autosize, 0);
}

const blobToDataUrl = (blob) =>
  new Promise((resolve, reject) => {
    const r = new FileReader();
    r.onload = () => resolve(r.result);
    r.onerror = () => reject(r.error);
    r.readAsDataURL(blob);
  });

async function sendImageBlob(blob) {
  const ext = ((blob.type || "image/png").split("/")[1] || "png").replace("jpeg", "jpg");
  const path = await invoke("save_pasted_image", { dataBase64: await blobToDataUrl(blob), ext });
  sendPaths([path]);
}

// 原生读取:返回 true 表示已处理(发了文件/图片)
async function pasteNative() {
  let c = null;
  try {
    c = await invoke("read_clipboard");
  } catch (_) {
    return false;
  }
  if (c.files && c.files.length) {
    sendPaths(c.files);
    return true;
  }
  if (c.picked && c.picked.length) {
    sendPaths([], c.picked);
    return true;
  }
  if (c.image) {
    sendPaths([c.image]);
    return true;
  }
  if (c.text) {
    insertText(c.text);
    return true;
  }
  return false;
}

$("composer-input").addEventListener("paste", (e) => {
  const cd = e.clipboardData;
  const img = cd && Array.from(cd.items || []).find((it) => it.kind === "file" && it.type.startsWith("image/"));
  const text = (cd && cd.getData("text/plain")) || "";
  if (IS_ANDROID) {
    // 手机:文字交给系统默认粘贴;图片自己接住发出去
    if (img && !text) {
      e.preventDefault();
      sendImageBlob(img.getAsFile()).catch((err) => toast(`粘贴图片失败: ${err}`, "err"));
    }
    return;
  }
  e.preventDefault();
  (async () => {
    if (await pasteNative()) return;
    if (img) {
      try {
        await sendImageBlob(img.getAsFile());
        return;
      } catch (err) {
        toast(`粘贴图片失败: ${err}`, "err");
      }
    }
    if (text) insertText(text);
    else if (cd && cd.files && cd.files.length) toast("剪贴板里有文件但拿不到路径,请改用拖拽或文件夹按钮选择", "err");
  })();
});

// 「粘贴」按钮:不用先点输入框,直接读剪贴板
async function pasteFromButton() {
  if (await pasteNative()) return;
  try {
    if (navigator.clipboard && navigator.clipboard.read) {
      const items = await navigator.clipboard.read();
      for (const it of items) {
        const t = it.types.find((x) => x.startsWith("image/"));
        if (t) {
          await sendImageBlob(await it.getType(t));
          return;
        }
      }
      for (const it of items) {
        if (it.types.includes("text/plain")) {
          const text = await (await it.getType("text/plain")).text();
          if (text) return insertText(text);
        }
      }
    } else if (navigator.clipboard && navigator.clipboard.readText) {
      const text = await navigator.clipboard.readText();
      if (text) return insertText(text);
    }
    toast("剪贴板是空的");
  } catch (err) {
    // 系统拒绝网页读剪贴板时,退回让用户在输入框里长按粘贴
    $("composer-input").focus();
    toast(IS_ANDROID ? "请在输入框里长按,选择「粘贴」" : `读取剪贴板失败: ${err}`);
  }
}
$("btn-paste").onclick = () => pasteFromButton().catch((e) => toast(`${e}`, "err"));

// 统一发送入口:路径数组(桌面)或 Android 选择器给的 picked 对象数组。
async function sendPaths(paths, picked = null) {
  if ((!paths || !paths.length) && (!picked || !picked.length)) return;
  if (!ensureTarget()) {
    pendingPaths = { paths, picked };
    const n = (picked && picked.length) || (paths || []).length;
    toast(`已选 ${n} 项,勾选设备后自动发送`);
    return;
  }
  const batch = newBatch();
  await Promise.allSettled(
    targetDevices().map((d) =>
      invoke("send_files", { fingerprint: d.fingerprint, paths: paths || [], picked, batch }).catch((err) =>
        logLine("error", `发给 ${d.alias} 失败: ${err}`)
      )
    )
  );
}

// 文件夹按钮 → 菜单:选择文件 / 选择文件夹
function toggleMenu(show) {
  const menu = $("attach-menu");
  const on = show ?? menu.classList.contains("hidden");
  menu.classList.toggle("hidden", !on);
  $("btn-attach").classList.toggle("active", on);
}
$("btn-attach").onclick = (e) => {
  e.stopPropagation();
  toggleMenu();
};
document.addEventListener("click", (e) => {
  if (!e.target.closest("#attach-menu")) toggleMenu(false);
});

// Android:自带的 PickerPlugin(SAF),返回 {uri,name,size,mimeType};
// 选文件夹时 name 带「文件夹/相对路径」前缀,接收端按文件夹分组。
async function androidPick(cmd) {
  const items = await invoke(cmd);
  return (items || []).filter((it) => it && it.uri);
}

$("pick-files").onclick = async () => {
  toggleMenu(false);
  try {
    if (IS_ANDROID) {
      const items = await androidPick("pick_files");
      if (items.length) sendPaths([], items);
    } else {
      const sel = await open({ multiple: true, directory: false });
      if (sel && sel.length) sendPaths(Array.isArray(sel) ? sel : [sel]);
    }
  } catch (err) {
    logLine("error", `选择文件失败: ${err}`);
  }
};

$("pick-folder").onclick = async () => {
  toggleMenu(false);
  try {
    if (IS_ANDROID) {
      const items = await androidPick("pick_folder");
      if (items.length) {
        sendPaths([], items);
      } else {
        toast("这个文件夹里没有文件");
      }
    } else {
      const sel = await open({ directory: true, multiple: true });
      if (sel && sel.length) sendPaths(Array.isArray(sel) ? sel : [sel]);
    }
  } catch (err) {
    logLine("error", `选择文件夹失败: ${err}`);
  }
};

// ---------- 拖拽传输(桌面):文件 / 照片 / 文件夹 直接拖进窗口 ----------

document.addEventListener("dragover", (e) => e.preventDefault());
document.addEventListener("drop", (e) => e.preventDefault());

// macOS 上 wry 用原生 NSDragging 接管拖拽,改用 Tauri 转发的事件做悬停高亮
listen("tauri://drag-enter", () => !draggingOut && $("stage").classList.add("drag-over"));
listen("tauri://drag-leave", () => $("stage").classList.remove("drag-over"));
listen("tauri://drag-drop", (e) => {
  $("stage").classList.remove("drag-over");
  if (draggingOut) return; // 从本窗口拖出又放回本窗口,不当作发送
  const paths = (e.payload && e.payload.paths) || [];
  if (!paths.length) {
    // 某些拖拽源(快速查看、网页图片)只给非文件 URL,wry 解不出路径
    logLine("error", "拖拽的内容未识别到文件路径,请改用文件夹按钮选择,或从 Finder 中拖动");
    return;
  }
  sendPaths(paths);
});

// ---------- 工具栏 ----------

$("btn-open-dir").onclick = () => invoke("open_received_dir").catch((e) => logLine("error", `${e}`));

// ---------- 灵动岛 ----------

async function enterCompact() {
  const win = getCurrentWindow();
  await win.setDecorations(false);
  await win.setAlwaysOnTop(true);
  await win.setSkipTaskbar(true);
  await win.setResizable(false);
  await win.setSize(new LogicalSize(COMPACT_W, COMPACT_H));

  try {
    const mon = await currentMonitor();
    const scale = mon.scaleFactor || 1;
    const x = Math.round(mon.position.x + (mon.size.width - COMPACT_W * scale) / 2);
    const y = Math.round(mon.position.y + 4 * scale);
    await win.setPosition(new PhysicalPosition(x, y));
  } catch (_) {
    await win.center();
  }

  document.body.classList.add("compact");
  $("island").classList.remove("hidden");
  $("island-status").textContent = islandText;
}

async function exitCompact() {
  const win = getCurrentWindow();
  document.body.classList.remove("compact");
  $("island").classList.add("hidden");
  await win.setAlwaysOnTop(false);
  await win.setSkipTaskbar(false);
  await win.setSize(new LogicalSize(760, 560));
  await win.setResizable(true);
  await win.setDecorations(true);
  await win.center();
}

$("btn-compact").onclick = enterCompact;
$("island-expand").onclick = exitCompact;
$("island").ondblclick = exitCompact;

// ---------- 接收弹窗 ----------

function answer(accept, pair) {
  $("receive-dialog").close();
  invoke("answer_receive", { accept, pair }).catch(() => {});
  pendingRequest = null;
}
$("req-accept").onclick = () => answer(true, false);
$("req-pair").onclick = () => answer(true, true);
$("req-decline").onclick = () => answer(false, false);

// ---------- 设置弹窗 ----------

$("btn-settings").onclick = async () => {
  try {
    const cfg = await invoke("get_config");
    $("set-alias").value = cfg.alias;
    $("set-dest").value = cfg.destination;
    $("set-fp").textContent = cfg.fingerprint;
    $("settings-dialog").showModal();
  } catch (e) {
    logLine("error", `读取设置失败: ${e}`);
  }
};
$("set-save").onclick = async () => {
  try {
    const alias = $("set-alias").value;
    const dest = $("set-dest").value;
    if (alias.trim()) await invoke("set_alias", { alias: alias.trim() });
    if (dest.trim()) await invoke("set_destination", { path: dest.trim() });
    $("settings-dialog").close();
  } catch (e) {
    logLine("error", `保存设置失败: ${e}`);
  }
};
$("set-close").onclick = () => $("settings-dialog").close();
$("set-clear").onclick = async () => {
  if (!confirm("清空所有设备的聊天记录?已收到的文件不会删除。")) return;
  history.clear();
  try {
    await invoke("clear_history");
  } catch (e) {
    logLine("error", `清空失败: ${e}`);
  }
  renderDevices();
  toast("聊天记录已清空");
};
$("set-browse").onclick = async () => {
  try {
    const dir = await open({ directory: true, multiple: false, defaultPath: $("set-dest").value || undefined });
    if (dir) $("set-dest").value = dir;
  } catch (e) {
    logLine("error", `选择目录失败: ${e}`);
  }
};

// ---------- 检查更新 ----------
// 启动 5 秒后检查一次,之后每 6 小时一次。同一版本点了「稍后」就不再弹条,
// 但设置里仍显示有新版本。

let updateInfo = null;
const UPDATE_EVERY = 6 * 3600 * 1000;

async function checkUpdate(manual = false) {
  const status = $("set-update-status");
  if (manual) status.textContent = "正在检查…";
  try {
    const u = await invoke("check_update");
    updateInfo = u;
    $("set-version").textContent = u.current;
    if (u.available) {
      status.textContent = `发现新版本 ${u.latest}`;
      const skipped = localStorage.getItem("skipUpdate") === u.latest;
      if (manual || !skipped) showUpdateBar(u);
    } else {
      status.textContent = "已是最新版本";
      if (manual) toast(`已是最新版本 ${u.current}`);
    }
  } catch (e) {
    status.textContent = "检查失败,稍后会自动重试";
    if (manual) toast(`${e}`, "err");
  }
}

function showUpdateBar(u) {
  $("update-title").textContent = `跨机中转 ${u.latest} 可以更新了(当前 ${u.current})`;
  $("update-notes").textContent = u.notes || "";
  $("update-bar").classList.remove("hidden");
}

$("update-go").onclick = () => {
  if (!updateInfo) return;
  invoke("open_update_url", { url: updateInfo.url || updateInfo.page })
    .then(() => toast(IS_ANDROID ? "已在浏览器中下载,完成后点开安装包即可覆盖安装" : "已打开下载页"))
    .catch((e) => toast(`打开下载失败: ${e}`, "err"));
  $("update-bar").classList.add("hidden");
};
$("update-later").onclick = () => {
  if (updateInfo) localStorage.setItem("skipUpdate", updateInfo.latest);
  $("update-bar").classList.add("hidden");
};
$("set-check-update").onclick = () => checkUpdate(true);
window.__TAURI__.app.getVersion().then((v) => ($("set-version").textContent = v)).catch(() => {});
setTimeout(() => checkUpdate(false), 5000);
setInterval(() => checkUpdate(false), UPDATE_EVERY);

// ---------- 事件 ----------

listen("devices-changed", (e) => {
  devices = e.payload;
  // 离线设备在「按设备」里仍可查看历史;只有发送目标在「全部/按类型」下失效时才清掉
  if (selectedFingerprint && !deviceByFp(selectedFingerprint) && !history.has(selectedFingerprint)) {
    selectedFingerprint = null;
  }
  if (!selectedFingerprint && devices.length === 1) selectedFingerprint = devices[0].fingerprint;
  renderDevices();
  flushPending();
});

listen("log", (e) => logLine(e.payload.category, e.payload.text));

listen("receive-request", (e) => {
  pendingRequest = e.payload;
  $("req-title").textContent = `${e.payload.alias} 想发给你 ${e.payload.files.length} 个文件 · ${fmtBytes(e.payload.totalBytes)}`;
  const ul = $("req-files");
  ul.innerHTML = "";
  for (const f of e.payload.files) {
    const li = el("li");
    li.append(kindBadge(f), el("span", "rf-name", f.name), el("span", "rf-size", fmtBytes(f.size)));
    ul.appendChild(li);
  }
  $("receive-dialog").showModal();
});

listen("receive-progress", (e) => {
  const p = e.payload;
  const key = `recv:${p.sessionId}`;
  const ratio = p.totalBytes ? p.doneBytes / p.totalBytes : 0;
  const msgs = history.get(p.fingerprint) || [];
  const existing = msgs.find((x) => x._key === key);
  const patch = {
    ratio,
    state: "sending",
    statusText: `${p.finishedFiles}/${p.totalFiles} · ${fmtBytes(p.doneBytes)} / ${fmtBytes(p.totalBytes)}  ${fmtSpeed(p.speedBps)}`,
    cancellable: true,
    onCancel: () => invoke("cancel_receive").catch(() => {}),
  };
  if (existing) {
    Object.assign(existing, patch);
    renderMessages();
  } else {
    pushMsg(p.fingerprint, {
      _key: key, dir: "in", kind: "files", alias: p.alias,
      files: [], ts: Date.now(), ...patch,
    });
  }
  islandText = `⬇ ${p.alias} ${Math.round(ratio * 100)}%`;
  $("island-status").textContent = islandText;
});

listen("receive-done", (e) => {
  const p = e.payload;
  const key = `recv:${p.sessionId}`;
  const msgs = history.get(p.fingerprint) || [];
  const existing = msgs.find((x) => x._key === key);
  const files = (p.files || []).map((f) => ({
    id: f.id, name: f.name, size: f.size,
    fileType: f.fileType, savedPath: f.savedPath,
  }));
  const patch = {
    files,
    ratio: 1,
    state: p.reason === "finished" ? "done" : "cancelled",
    statusText: p.reason === "finished" ? "" : `已取消(失败 ${p.failedFiles} 个)`,
    cancellable: false,
  };
  if (existing) {
    Object.assign(existing, patch);
    renderMessages();
  } else {
    pushMsg(p.fingerprint, {
      _key: key, dir: "in", kind: "files", alias: p.alias, ts: Date.now(), ...patch,
    });
  }
  if (p.reason === "finished") toast(`已收到 ${p.receivedFiles} 个文件`);
  islandText = `⬇ ${p.alias} 完成`;
  $("island-status").textContent = islandText;
});

listen("send-progress", (e) => {
  const p = e.payload;
  const key = `send:${p.sendId}`;
  const ratio = p.totalBytes ? p.doneBytes / p.totalBytes : 0;
  const zh = { sending: "发送中", done: "已送达", failed: "发送失败", cancelled: "已取消" }[p.state] || p.state;
  const msgs = history.get(p.fingerprint) || [];
  const existing = msgs.find((x) => x._key === key);
  const files = (p.files || []).map((f) => ({
    id: f.id, name: f.name, size: f.size,
    fileType: f.fileType, savedPath: f.savedPath,
  }));
  const patch = {
    ratio,
    state: p.state,
    statusText: p.kind === "text"
      ? (p.state === "done" ? "" : zh + (p.error ? ` · ${p.error}` : ""))
      : p.state === "done" ? ""
      : `${zh} · ${fmtBytes(p.doneBytes)} / ${fmtBytes(p.totalBytes)}  ${fmtSpeed(p.speedBps)}${p.error ? " · " + p.error : ""}`,
    cancellable: p.state === "sending",
    onCancel: () => invoke("cancel_send", { sendId: p.sendId }).catch(() => {}),
  };
  if (p.batch) patch.batch = p.batch;
  if (files.length) patch.files = files;
  if (existing) {
    Object.assign(existing, patch);
    renderMessages();
  } else if (p.kind === "text") {
    // message-received 已经建了气泡:按同文内容认领,避免重复气泡
    const dup = msgs.find((x) => x.dir === "out" && x.kind === "text" && x.text === (p.textPreview || "") && !x._key && !x._old);
    if (dup) {
      dup._key = key;
      Object.assign(dup, patch);
      renderMessages();
    } else {
      pushMsg(p.fingerprint, {
        _key: key, dir: "out", kind: "text", alias: p.alias,
        text: p.textPreview || "", ts: Date.now(), ...patch,
      });
    }
  } else {
    pushMsg(p.fingerprint, {
      _key: key, dir: "out", kind: "files", alias: p.alias,
      files, ts: Date.now(), ...patch,
    });
  }
  islandText = `⬆ ${p.alias} ${p.state === "sending" ? Math.round(ratio * 100) + "%" : zh}`;
  $("island-status").textContent = islandText;
});

listen("message-received", (e) => {
  const p = e.payload;
  pushMsg(p.fingerprint, {
    dir: p.direction === "out" ? "out" : "in",
    kind: "text",
    alias: p.alias,
    text: p.text,
    ts: p.tsMs || Date.now(),
    state: "done",
    statusText: "",
  });
});

// ---------- 初始化 ----------

scanning = true;
setTimeout(() => {
  scanning = false;
  renderDevices();
}, 4000);

invoke("list_devices")
  .then((list) => {
    devices = list;
    if (!selectedFingerprint && devices.length === 1) selectedFingerprint = devices[0].fingerprint;
    renderDevices();
  })
  .catch(() => {});

loadHistory().then(() => {
  // 「按设备」默认选中最近有往来的设备
  if (!selectedFingerprint) {
    const k = knownDevices();
    if (k.length === 1) selectedFingerprint = k[0].fp;
  }
  setView("all");
});
