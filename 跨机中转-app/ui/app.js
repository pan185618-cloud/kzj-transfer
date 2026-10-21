// 跨机中转 UI — Tauri v2 global API (withGlobalTauri).
const { invoke } = window.__TAURI__.core;
const { listen } = window.__TAURI__.event;
const { open, save } = window.__TAURI__.dialog;
const { getCurrentWindow, LogicalSize, PhysicalPosition, currentMonitor } = window.__TAURI__.window;

const $ = (id) => document.getElementById(id);

const COMPACT_W = 320;
const COMPACT_H = 36;

let devices = [];
let selectedFingerprint = null;
let pendingRequest = null;
let islandText = "中转";
// 消息历史: fingerprint -> [{dir,kind,text,files,sessionId,sendId,ts,state,ratio}]
const history = new Map();
let typeView = false; // 按类型分类显示模式
let pendingPaths = null; // 拖拽进来但还没选设备时暂存的路径
let dragDepth = 0;       // 拖拽悬停计数,避免在子元素间移动时闪烁

function fmtBytes(n) {
  if (n < 1024) return `${n} B`;
  if (n < 1024 * 1024) return `${(n / 1024).toFixed(1)} KB`;
  if (n < 1024 * 1024 * 1024) return `${(n / 1024 / 1024).toFixed(1)} MB`;
  return `${(n / 1024 / 1024 / 1024).toFixed(2)} GB`;
}
const fmtSpeed = (bps) => (bps > 0 ? `${fmtBytes(Math.round(bps))}/s` : "");
const fmtTime = (ts) => {
  const d = new Date(ts);
  return `${String(d.getHours()).padStart(2, "0")}:${String(d.getMinutes()).padStart(2, "0")}`;
};

function logLine(category, text) {
  const el = $("log");
  const line = document.createElement("div");
  const cat = document.createElement("span");
  cat.className = "cat";
  cat.textContent = `[${category}] `;
  line.appendChild(cat);
  line.appendChild(document.createTextNode(text));
  el.appendChild(line);
  el.scrollTop = el.scrollHeight;
}

// ---------- 设备 chip 栏 ----------

function renderDeviceBar() {
  const bar = $("device-bar");
  // 保留 全部/按类型/+IP,重建设备 chip
  bar.querySelectorAll(".chip.dev").forEach((c) => c.remove());
  const addBtn = $("chip-add");
  for (const d of devices) {
    const chip = document.createElement("button");
    chip.className = "chip dev" + (d.fingerprint === selectedFingerprint ? " active" : "");
    chip.title = `${d.alias} · ${d.host}:${d.port}`;
    const dot = document.createElement("span");
    dot.className = "dot";
    chip.appendChild(dot);
    chip.appendChild(document.createTextNode(d.alias));
    chip.onclick = () => selectDevice(d.fingerprint);
    bar.insertBefore(chip, addBtn);
  }
  $("chip-all").classList.toggle("active", selectedFingerprint === null && !typeView);
  $("chip-type").classList.toggle("active", typeView);
  // 无设备时给出明显提示
  $("device-hint").classList.toggle("hidden", devices.length > 0 || hasMessages());
}

function selectDevice(fp) {
  selectedFingerprint = fp;
  typeView = false;
  renderDeviceBar();
  renderMessages();
  // 若拖拽进来的文件还在等选设备,选定后自动发出
  if (pendingPaths && pendingPaths.length) {
    const paths = pendingPaths;
    pendingPaths = null;
    sendPaths(paths);
  }
}

function hasMessages() {
  for (const msgs of history.values()) if (msgs.length) return true;
  return false;
}

$("chip-all").onclick = () => {
  selectedFingerprint = null;
  typeView = false;
  renderDeviceBar();
  renderMessages();
};
$("chip-type").onclick = () => {
  typeView = !typeView;
  selectedFingerprint = null;
  renderDeviceBar();
  renderMessages();
};

// ---------- 消息区渲染 ----------

const URL_RE = /https?:\/\/[^\s"'<>]+/g;

function linkify(text) {
  const frag = document.createDocumentFragment();
  let last = 0;
  for (const m of text.matchAll(URL_RE)) {
    if (m.index > last) frag.appendChild(document.createTextNode(text.slice(last, m.index)));
    const a = document.createElement("a");
    a.href = "#";
    a.textContent = m[0];
    a.onclick = (e) => {
      e.preventDefault();
      invoke("open_link", { url: m[0] }).catch((err) => logLine("error", `${err}`));
    };
    frag.appendChild(a);
    last = m.index + m[0].length;
  }
  if (last < text.length) frag.appendChild(document.createTextNode(text.slice(last)));
  return frag;
}

function typeTag(f) {
  const t = (f.fileType || f.file_type || "").toLowerCase();
  if (t.startsWith("image/")) return "[图片]";
  if (t.startsWith("video/")) return "[视频]";
  if (t.startsWith("audio/")) return "[音频]";
  if (t.includes("pdf")) return "[PDF]";
  if (t.startsWith("text/")) return "[文本]";
  if (t.includes("zip") || t.includes("compressed") || t.includes("tar")) return "[压缩]";
  if (t.includes("word") || t.includes("officedocument")) return "[文档]";
  const ext = (f.name || "").split(".").pop();
  return ext && ext !== f.name ? `[${ext.toUpperCase()}]` : "[文件]";
}

function typeIcon(f) {
  const t = (f.fileType || f.file_type || "").toLowerCase();
  if (t.startsWith("image/")) return "🖼";
  if (t.startsWith("video/")) return "🎬";
  if (t.startsWith("audio/")) return "🎵";
  if (t.includes("pdf")) return "📕";
  if (t.startsWith("text/")) return "📄";
  if (t.includes("zip") || t.includes("compressed") || t.includes("tar")) return "🗜";
  return "📦";
}

// 文件分类(按 typeTag 大类)
function typeGroup(f) {
  const tag = typeTag(f);
  return tag;
}

// ---------- 文件夹分组显示 ----------
// name 形如 "目录名/子路径/文件"(来自 collect_path 的 strip_prefix):
// 第一段是顶层目录名 → 该文件归入这个文件夹组;不带 "/" 的是顶层散文件。

function isFolderMsg(m) {
  return m.kind === "files" && (m.files || []).some((f) => (f.name || "").includes("/"));
}

// 按第一段路径分组,保持文件原有顺序
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

// 文件夹卡片:📁 文件夹名 + N 个文件/总大小,点击打开文件夹;
// 下方缩进列出组内文件(相对路径),单击仍可打开单个文件。
function bubbleForFolder(g, dir) {
  const wrap = document.createElement("div");
  wrap.className = "f-folder";

  const head = document.createElement("div");
  head.className = "f-card f-folder-head";
  const icon = document.createElement("span");
  icon.className = "f-icon";
  icon.textContent = "📁";
  const info = document.createElement("div");
  info.className = "f-info";
  const name = document.createElement("div");
  name.className = "f-name";
  name.textContent = `[文件夹] ${g.name}`;
  const sub = document.createElement("div");
  sub.className = "f-sub";
  sub.textContent = `${g.files.length} 个文件 · ${fmtBytes(g.size)}`;
  info.append(name, sub);
  head.append(icon, info);
  if (g.dirPath) {
    head.classList.add("openable");
    head.title = g.dirPath;
    head.onclick = () =>
      invoke("open_file", { path: g.dirPath }).catch((e) => logLine("error", `打开失败: ${e}`));
  }
  wrap.appendChild(head);

  const list = document.createElement("div");
  list.className = "f-folder-items";
  for (const f of g.files) {
    const rel = { ...f, name: f.name.slice(g.name.length + 1) };
    const item = bubbleForFile(rel, dir);
    item.classList.add("f-subitem");
    list.appendChild(item);
  }
  wrap.appendChild(list);
  return wrap;
}

function bubbleForFile(f, dir) {
  const card = document.createElement("div");
  card.className = "f-card";
  const icon = document.createElement("span");
  icon.className = "f-icon";
  icon.textContent = typeIcon(f);
  const info = document.createElement("div");
  info.className = "f-info";
  const name = document.createElement("div");
  name.className = "f-name";
  name.textContent = `${typeTag(f)} ${f.name}`;
  const sub = document.createElement("div");
  sub.className = "f-sub";
  sub.textContent = fmtBytes(f.size || 0);
  info.append(name, sub);
  const path = f.savedPath || f.saved_path;
  const isImage = (f.fileType || f.file_type || "").toLowerCase().startsWith("image/");
  // 图片:在气泡里直接显示缩略图(懒加载,失败回落到图标)
  if (isImage && path) {
    card.classList.add("has-thumb");
    const thumb = document.createElement("img");
    thumb.className = "f-thumb";
    thumb.alt = f.name || "";
    thumb.loading = "lazy";
    thumb.onerror = () => {
      thumb.remove();
      card.classList.remove("has-thumb");
      card.prepend(icon);
    };
    invoke("read_image_preview", { path })
      .then((url) => {
        thumb.src = url;
        card.prepend(thumb);
      })
      .catch(() => {
        thumb.remove();
        card.classList.remove("has-thumb");
        card.prepend(icon);
      });
    card.append(info);
  } else {
    card.append(icon, info);
  }
  if (path) {
    card.classList.add("openable");
    card.title = path;
    card.onclick = () =>
      invoke("open_file", { path }).catch((e) => logLine("error", `打开失败: ${e}`));
    // 另存为:弹出系统保存对话框,把文件复制到用户选的位置
    const saveBtn = document.createElement("button");
    saveBtn.className = "f-save-as";
    saveBtn.textContent = "另存为";
    saveBtn.title = "保存到其他位置";
    saveBtn.onclick = async (e) => {
      e.stopPropagation();
      try {
        const dot = (f.name || "").lastIndexOf(".");
        const ext = dot > 0 ? f.name.slice(dot + 1) : null;
        // 扩展名合法才加过滤器,避免对话框报错
        const filters = /^[a-zA-Z0-9]{1,10}$/.test(ext || "")
          ? [{ name: `${ext.toUpperCase()} 文件`, extensions: [ext] }, { name: "所有文件", extensions: ["*"] }]
          : undefined;
        const dst = await save({ defaultPath: f.name, filters });
        if (!dst) return;
        await invoke("copy_file_to", { src: path, dst });
        logLine("save", `已另存为: ${dst}`);
      } catch (err) {
        logLine("error", `另存为失败: ${err}`);
      }
    };
    card.appendChild(saveBtn);
  }
  return card;
}

function msgNode(m) {
  const li = document.createElement("li");
  li.className = `msg ${m.dir}`;

  const meta = document.createElement("div");
  meta.className = "m-meta";
  meta.textContent = `${m.dir === "in" ? m.alias + " · " : ""}${fmtTime(m.ts)}${m.dir === "out" ? " · 发给 " + m.alias : ""}`;
  li.appendChild(meta);

  const bubble = document.createElement("div");
  bubble.className = "bubble";

  if (m.kind === "text") {
    bubble.appendChild(linkify(m.text));
  } else if (isFolderMsg(m)) {
    for (const g of groupByFolder(m.files)) {
      bubble.appendChild(g.name === null ? bubbleForFile(g.files[0], m.dir) : bubbleForFolder(g, m.dir));
    }
  } else {
    for (const f of m.files || []) bubble.appendChild(bubbleForFile(f, m.dir));
  }

  // 进度条与状态(传输中)
  if (m.state && m.state !== "done") {
    const bar = document.createElement("div");
    bar.className = "bar";
    bar.innerHTML = "<div></div>";
    bar.firstChild.style.width = `${Math.min(100, Math.round((m.ratio || 0) * 100))}%`;
    bubble.appendChild(bar);
    const status = document.createElement("div");
    status.className = "t-status";
    status.textContent = m.statusText || "";
    bubble.appendChild(status);
    if (m.cancellable) {
      const btn = document.createElement("button");
      btn.className = "t-cancel";
      btn.textContent = "取消";
      btn.onclick = m.onCancel || (() => {});
      bubble.appendChild(btn);
    }
  } else if (m.statusText) {
    const status = document.createElement("div");
    status.className = "t-status";
    status.textContent = m.statusText;
    bubble.appendChild(status);
  }

  li.appendChild(bubble);
  return li;
}

function renderMessages() {
  const ul = $("msg-list");
  ul.innerHTML = "";
  const has = renderInto(ul);
  $("empty-hint").classList.toggle("hidden", has || devices.length === 0);
  $("device-hint").classList.toggle("hidden", devices.length > 0 || has);
  // 滚到底部
  $("stage").scrollTop = $("stage").scrollHeight;
}

// 返回是否渲染了至少一条
function renderInto(ul) {
  let count = 0;
  if (typeView) {
    // 按类型分组:收集所有文件消息
    const groups = new Map();
    for (const [fp, msgs] of history) {
      if (selectedFingerprint && fp !== selectedFingerprint) continue;
      for (const m of msgs) {
        if (m.kind !== "files" || !m.files) continue;
        for (const f of m.files) {
          const g = typeGroup(f);
          if (!groups.has(g)) groups.set(g, []);
          groups.get(g).push({ m, f });
        }
      }
    }
    const keys = [...groups.keys()].sort();
    for (const k of keys) {
      const head = document.createElement("li");
      head.className = "type-head";
      head.textContent = `${k} (${groups.get(k).length})`;
      ul.appendChild(head);
      for (const { m, f } of groups.get(k)) {
        const node = msgNode({ ...m, files: [f] });
        ul.appendChild(node);
        count++;
      }
    }
    // 文本消息不分组,追加在最后? 或也显示?按需求"按文件类型分类"主要针对文件;文本归入[文本]已由 typeTag 处理,但 text 消息没有 files — 单独一组
    const texts = [];
    for (const [fp, msgs] of history) {
      if (selectedFingerprint && fp !== selectedFingerprint) continue;
      for (const m of msgs) if (m.kind === "text") texts.push(m);
    }
    if (texts.length) {
      const head = document.createElement("li");
      head.className = "type-head";
      head.textContent = `[文本/链接] (${texts.length})`;
      ul.appendChild(head);
      for (const m of texts) {
        ul.appendChild(msgNode(m));
        count++;
      }
    }
    return count > 0;
  }

  // 设备视图
  for (const [fp, msgs] of history) {
    if (selectedFingerprint && fp !== selectedFingerprint) continue;
    for (const m of msgs) {
      ul.appendChild(msgNode(m));
      count++;
    }
  }
  return count > 0;
}

// ---------- 历史记录 ----------

function pushMsg(fp, msg) {
  if (!history.has(fp)) history.set(fp, []);
  history.get(fp).push(msg);
  renderMessages();
}

function updateTransfer(key, fp, patch) {
  const msgs = history.get(fp);
  if (!msgs) return;
  const m = msgs.find((x) => x._key === key);
  if (m) Object.assign(m, patch);
  renderMessages();
}

// ---------- 发送 ----------

async function doSendText() {
  const text = $("composer-input").value.trim();
  if (!text) return;
  if (!selectedFingerprint) {
    // 未选设备:若只有一台在线设备则默认选它
    if (devices.length === 1) {
      selectDevice(devices[0].fingerprint);
    } else {
      logLine("send", "请先在顶部选择目标设备");
      $("device-hint").classList.remove("hidden");
      return;
    }
  }
  try {
    await invoke("send_text", { fingerprint: selectedFingerprint, text });
    $("composer-input").value = "";
    autosize();
  } catch (err) {
    logLine("send", `发送失败: ${err}`);
  }
}

$("composer-send").onclick = doSendText;
$("composer-input").addEventListener("keydown", (e) => {
  if (e.key === "Enter" && !e.shiftKey) {
    e.preventDefault();
    doSendText();
  }
});

// 输入框自动增高
function autosize() {
  const ta = $("composer-input");
  ta.style.height = "auto";
  ta.style.height = Math.min(ta.scrollHeight, 120) + "px";
}
$("composer-input").addEventListener("input", autosize);

// 粘贴:若内容是纯链接也直接可发(已是文本),无需特殊处理;这里做体验优化:含链接时提示 🔗
$("composer-input").addEventListener("paste", () => setTimeout(autosize, 0));

// 统一发送入口:文件 / 照片 / 文件夹的路径数组 → 发给当前选中设备。
// 未选设备时:只有一台在线设备则自动选中;否则暂存路径,等用户选定设备后自动发出。
async function sendPaths(paths) {
  if (!paths || paths.length === 0) return;
  if (!selectedFingerprint) {
    if (devices.length === 1) {
      selectDevice(devices[0].fingerprint);
    } else {
      pendingPaths = paths;
      logLine("send", "请先在顶部选择目标设备(已暂存待发送的文件)");
      $("device-hint").classList.remove("hidden");
      return;
    }
  }
  try {
    await invoke("send_files", { fingerprint: selectedFingerprint, paths });
  } catch (err) {
    logLine("send", `发送失败: ${err}`);
  }
}

// 📎 发文件(不传 filters = 全部类型:图片/视频/文档/压缩包等)
$("btn-attach").onclick = async () => {
  const sel = await open({ multiple: true, directory: false });
  if (!sel || sel.length === 0) return;
  sendPaths(Array.isArray(sel) ? sel : [sel]);
};

// 📁 发整个文件夹(自动递归里面所有文件)
$("btn-attach-dir").onclick = async () => {
  const sel = await open({ directory: true, multiple: true });
  if (!sel || sel.length === 0) return;
  sendPaths(Array.isArray(sel) ? sel : [sel]);
};

// ---------- 拖拽传输:文件 / 照片 / 文件夹 直接拖进窗口 ----------

// 阻止浏览器默认行为(否则拖进来会在窗口里直接打开文件)
document.addEventListener("dragover", (e) => e.preventDefault());
document.addEventListener("drop", (e) => e.preventDefault());

// 悬停高亮(计数器避免在子元素间移动时闪烁)
document.addEventListener("dragenter", (e) => {
  e.preventDefault();
  if (e.dataTransfer && Array.from(e.dataTransfer.types).includes("Files")) {
    dragDepth++;
    $("stage").classList.add("drag-over");
  }
});
document.addEventListener("dragleave", (e) => {
  e.preventDefault();
  if (dragDepth > 0) dragDepth--;
  if (dragDepth === 0) $("stage").classList.remove("drag-over");
});

// Tauri 原生拖放事件:拿到真实路径,文件和文件夹都支持
listen("tauri://drag-drop", (e) => {
  dragDepth = 0;
  $("stage").classList.remove("drag-over");
  const paths = (e.payload && e.payload.paths) || [];
  if (!paths.length) return;
  sendPaths(paths);
});

// 🔗 本机打开输入中的链接(不发送)
$("composer-open-link").onclick = async () => {
  const text = $("composer-input").value;
  const urls = text.match(URL_RE) || [];
  if (!urls.length) {
    logLine("error", "输入中没有识别到 http/https 链接");
    return;
  }
  for (const u of urls.slice(0, 10)) {
    try {
      await invoke("open_link", { url: u });
    } catch (err) {
      logLine("error", `打开失败: ${err}`);
    }
  }
};

// ---------- 工具栏 ----------

$("chip-add").onclick = () => {
  $("ip-pop").classList.toggle("hidden");
  if (!$("ip-pop").classList.contains("hidden")) $("ip-input").focus();
};
$("btn-add-ip").onclick = async () => {
  const ip = $("ip-input").value.trim();
  if (!ip) return;
  try {
    await invoke("add_device_ip", { ip, port: null });
    $("ip-input").value = "";
    $("ip-pop").classList.add("hidden");
    logLine("discover", `已添加 ${ip}`);
  } catch (err) {
    logLine("error", `${err}`);
  }
};
$("ip-input").addEventListener("keydown", (e) => {
  if (e.key === "Enter") $("btn-add-ip").click();
});
$("btn-refresh").onclick = () => invoke("refresh_devices").catch(() => {});
$("btn-open-dir").onclick = () => invoke("open_received_dir").catch((e) => logLine("error", `${e}`));

// 点击空白处关弹层
document.addEventListener("click", (e) => {
  if (!e.target.closest("#ip-pop") && !e.target.closest("#chip-add")) {
    $("ip-pop").classList.add("hidden");
  }
});

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
$("set-browse").onclick = async () => {
  try {
    const dir = await open({ directory: true, multiple: false, defaultPath: $("set-dest").value || undefined });
    if (dir) $("set-dest").value = dir;
  } catch (e) {
    logLine("error", `选择目录失败: ${e}`);
  }
};

// ---------- 事件 ----------

listen("devices-changed", (e) => {
  devices = e.payload;
  if (selectedFingerprint && !devices.some((d) => d.fingerprint === selectedFingerprint)) {
    selectedFingerprint = null;
  }
  renderDeviceBar();
});

listen("log", (e) => logLine(e.payload.category, e.payload.text));

listen("receive-request", (e) => {
  pendingRequest = e.payload;
  $("req-title").textContent = `${e.payload.alias} 想发送 ${e.payload.files.length} 个文件 (${fmtBytes(e.payload.totalBytes)})`;
  const ul = $("req-files");
  ul.innerHTML = "";
  for (const f of e.payload.files) {
    const li = document.createElement("li");
    li.textContent = `${typeTag(f)} ${f.name} — ${fmtBytes(f.size)}`;
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
    statusText: `${p.finishedFiles}/${p.totalFiles} · ${fmtBytes(p.doneBytes)}/${fmtBytes(p.totalBytes)} ${fmtSpeed(p.speedBps)}`,
    cancellable: true,
    onCancel: () => invoke("cancel_receive").catch(() => {}),
  };
  if (existing) {
    Object.assign(existing, patch);
  } else {
    pushMsg(p.fingerprint, {
      _key: key, dir: "in", kind: "files", alias: p.alias,
      files: [], ts: Date.now(), ...patch,
    });
  }
  renderMessages();
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
    statusText: p.reason === "finished"
      ? `已接收 ${p.receivedFiles} 个 → ${p.savedTo}`
      : `已取消(失败 ${p.failedFiles} 个)`,
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
  islandText = `⬇ ${p.alias} 完成`;
  $("island-status").textContent = islandText;
});

listen("send-progress", (e) => {
  const p = e.payload;
  const key = `send:${p.sendId}`;
  const ratio = p.totalBytes ? p.doneBytes / p.totalBytes : 0;
  const zh = { sending: "发送中", done: "完成", failed: "失败", cancelled: "已取消" }[p.state] || p.state;
  const msgs = history.get(p.fingerprint) || [];
  const existing = msgs.find((x) => x._key === key);
  const files = (p.files || []).map((f) => ({
    id: f.id, name: f.name, size: f.size,
    fileType: f.fileType, savedPath: f.savedPath,
  }));
  const patch = {
    ratio,
    state: p.state,
    statusText: p.kind === "text" ? zh
      : `${zh} · ${fmtBytes(p.doneBytes)}/${fmtBytes(p.totalBytes)} ${fmtSpeed(p.speedBps)}${p.error ? " · " + p.error : ""}`,
    cancellable: p.state === "sending",
    onCancel: () => invoke("cancel_send", { sendId: p.sendId }).catch(() => {}),
  };
  if (files.length) patch.files = files;
  if (existing) {
    Object.assign(existing, patch);
    renderMessages();
  } else if (p.kind === "text") {
    // message-received 已经建了气泡:按同文内容认领,避免重复气泡
    const dup = msgs.find((x) => x.dir === "out" && x.kind === "text" && x.text === (p.textPreview || "") && !x._key);
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

invoke("list_devices")
  .then((list) => {
    devices = list;
    renderDeviceBar();
  })
  .catch(() => {});

renderMessages();
