// Android 原生拖入/系统分享队列 + 最近截图建议。桌面端不请求照片权限。
if (IS_ANDROID) (() => {
  let state = { enabled: false, access: "none" };
  let screenshot = null;
  let refreshing = false, refreshAgain = false, changingPermission = false, sendingScreenshot = false;
  let expiryTimer = null;
  const handledImports = new Set(), waitingImports = new Set(), dismissed = new Set();

  window.renderScreenshotTarget = () => {
    const targets = targetDevices();
    $("screenshot-target").textContent = targets.length
      ? `发给 ${targets.map((d) => d.alias).join("、")}` : "先选择接收设备";
  };

  function renderState(next) {
    state = next;
    const candidate = next.enabled && next.screenshot;
    const available = candidate && candidate.expiresAt > Date.now() && !dismissed.has(candidate.uri);
    const previous = screenshot?.uri;
    screenshot = available ? candidate : null;
    clearTimeout(expiryTimer);
    $("screenshot-suggestion").classList.toggle("hidden", !screenshot);
    $("screenshot-onboarding").classList.toggle("hidden", next.enabled || !!localStorage.getItem("screenshotIntroDismissed"));
    $("set-screenshot").textContent = next.enabled ? "关闭" : "开启";
    $("set-screenshot-permission").classList.toggle("hidden", !next.enabled || next.access === "full");
    $("set-screenshot-status").textContent = !next.enabled ? "开启后，回到 App 提示最近三分钟的截图"
      : next.access === "full" ? "已开启 · 截图后回到 App 即可发送"
      : next.access === "limited" ? "仅可读所选照片，新截图可能无法提示"
      : "照片权限已关闭，可重新授权；仍可拖入或分享图片";
    if (screenshot) {
      const uri = screenshot.uri;
      if (previous !== uri) {
        const image = $("screenshot-preview");
        image.removeAttribute("src");
        invoke("read_image_preview", { path: uri }).then((url) => {
          if (screenshot?.uri === uri) image.src = url;
        }).catch(() => {});
      }
      expiryTimer = setTimeout(() => {
        screenshot = null;
        $("screenshot-suggestion").classList.add("hidden");
      }, Math.max(0, screenshot.expiresAt - Date.now()));
    }
    window.renderScreenshotTarget();
  }

  async function acknowledge(id = null, uri = null) {
    await invoke("acknowledge_quick_send", { id, uri });
  }

  async function consumeImports(imports) {
    for (const entry of imports || []) {
      if (handledImports.has(entry.id)) {
        // ACK 失败只重试确认，不重复发送。
        await acknowledge(entry.id);
        continue;
      }
      const items = (entry.items || []).filter((item) => item.uri);
      if (items.length && !targetDevices().length) {
        if (!waitingImports.has(entry.id)) {
          waitingImports.add(entry.id);
          ensureTarget();
          toast(`已接到 ${items.length} 张图片，选择设备后发送`);
        }
        continue; // 原生队列仍保留；即使此时重启也不会丢图。
      }
      if (items.length && !(await sendPaths([], items))) continue;
      handledImports.add(entry.id);
      waitingImports.delete(entry.id);
      if (entry.error) toast(entry.error, "err");
      await acknowledge(entry.id);
    }
  }

  window.refreshAndroidQuickSend = async () => {
    if (document.hidden) return;
    if (refreshing) { refreshAgain = true; return; }
    refreshing = true;
    try {
      do {
        refreshAgain = false;
        const next = await invoke("android_quick_send_state");
        if (!next) break;
        renderState(next);
        await consumeImports(next.imports);
      } while (refreshAgain);
    } catch (error) { console.warn("读取截图快捷入口失败", error); }
    finally { refreshing = false; }
  };

  async function changeEnabled(enabled) {
    if (changingPermission) return;
    changingPermission = true;
    for (const id of ["screenshot-enable", "set-screenshot", "set-screenshot-permission", "pick-screenshot"]) $(id).disabled = true;
    try {
      const next = await invoke("set_screenshot_suggestions", { enabled });
      renderState(next);
      if (enabled && next.access !== "full") toast(next.access === "limited"
        ? "目前只能读取所选照片。要提示新截图，请允许读取全部照片；也可直接拖入或分享图片。"
        : "未获得照片权限，仍可直接拖入截图，或从系统分享菜单发送。", "info");
      else if (enabled && !screenshot) toast("已开启，截完图回到这里就能看到发送提示");
      await consumeImports(next.imports);
    } catch (error) { toast(`设置截图提醒失败: ${error}`, "err"); }
    finally {
      changingPermission = false;
      for (const id of ["screenshot-enable", "set-screenshot", "set-screenshot-permission", "pick-screenshot"]) $(id).disabled = false;
    }
  }

  $("screenshot-enable").onclick = () => changeEnabled(true);
  $("set-screenshot").onclick = () => changeEnabled(!state.enabled);
  $("set-screenshot-permission").onclick = () => changeEnabled(true);
  $("screenshot-onboarding-dismiss").onclick = () => {
    localStorage.setItem("screenshotIntroDismissed", "1");
    $("screenshot-onboarding").classList.add("hidden");
  };
  $("pick-screenshot").onclick = async () => {
    toggleMenu(false);
    if (!state.enabled || state.access !== "full") return changeEnabled(true);
    await window.refreshAndroidQuickSend();
    if (!screenshot) toast("最近三分钟没有新截图；截好后回到这里即可发送");
  };
  $("screenshot-dismiss").onclick = async () => {
    if (!screenshot || sendingScreenshot) return;
    const uri = screenshot.uri;
    dismissed.add(uri);
    screenshot = null;
    $("screenshot-suggestion").classList.add("hidden");
    try { await acknowledge(null, uri); } catch (error) { console.warn(error); }
  };
  $("screenshot-send").onclick = async () => {
    if (!screenshot || sendingScreenshot || !ensureTarget()) return;
    const item = screenshot;
    sendingScreenshot = true;
    $("screenshot-send").disabled = true;
    try {
      if (await sendPaths([], [item])) {
        dismissed.add(item.uri);
        if (screenshot?.uri === item.uri) {
          screenshot = null;
          $("screenshot-suggestion").classList.add("hidden");
        }
        await acknowledge(null, item.uri);
      }
    } catch (error) { toast(`发送截图失败: ${error}`, "err"); }
    finally { sendingScreenshot = false; $("screenshot-send").disabled = false; }
  };
  window.addEventListener("android-image-drag", (event) => $("stage").classList.toggle("drag-over", !!event.detail));
  window.addEventListener("android-quick-change", window.refreshAndroidQuickSend);
  window.addEventListener("focus", window.refreshAndroidQuickSend);
  document.addEventListener("visibilitychange", () => {
    if (!document.hidden) window.refreshAndroidQuickSend();
  });
  window.refreshAndroidQuickSend();
})();
