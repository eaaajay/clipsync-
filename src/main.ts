import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { getCurrentWindow } from "@tauri-apps/api/window";
import "./styles.css";

interface Config {
  webdavUrl: string;
  username: string;
  password: string;
  deviceId: string;
  hotkey: string;
}

// ---------- 快捷键录制 ----------
let hotkeyRaw = "";

const KEY_MAP: Record<string, string> = {
  Space: "Space",
  Enter: "Enter",
  Tab: "Tab",
  Backquote: "Backquote",
  Minus: "Minus",
  Equal: "Equal",
  BracketLeft: "BracketLeft",
  BracketRight: "BracketRight",
  Backslash: "Backslash",
  Semicolon: "Semicolon",
  Quote: "Quote",
  Comma: "Comma",
  Period: "Period",
  Slash: "Slash",
  ArrowUp: "Up",
  ArrowDown: "Down",
  ArrowLeft: "Left",
  ArrowRight: "Right",
};

function keyName(e: KeyboardEvent): string | null {
  const c = e.code;
  if (/^Key[A-Z]$/.test(c)) return c.slice(3);
  if (/^Digit[0-9]$/.test(c)) return c.slice(5);
  if (/^F([1-9]|1[0-2])$/.test(c)) return c;
  return KEY_MAP[c] ?? null;
}

function accelFromEvent(e: KeyboardEvent): string | null {
  const mods: string[] = [];
  if (e.metaKey) mods.push(IS_ANDROID ? "Ctrl" : "CmdOrCtrl");
  if (e.ctrlKey) mods.push("Ctrl");
  if (e.altKey) mods.push("Alt");
  if (e.shiftKey) mods.push("Shift");
  if (!mods.length) return null;
  const k = keyName(e);
  if (!k) return null;
  return [...mods, k].join("+");
}

function prettyHotkey(a: string): string {
  if (!a) return "";
  return a
    .replace(/CmdOrCtrl/g, "⌘")
    .replace(/Command/g, "⌘")
    .replace(/Shift/g, "⇧")
    .replace(/Alt/g, "⌥")
    .replace(/Ctrl/g, "⌃")
    .replace(/\+/g, "");
}

function renderHotkey() {
  const el = $<HTMLInputElement>("#hotkey");
  el.value = prettyHotkey(hotkeyRaw);
}

async function refreshA11y() {
  if (!IS_ANDROID) return;
  const row = document.getElementById("a11y-row");
  if (!row) return;
  row.style.display = "flex";
  try {
    const enabled = await invoke<boolean>("accessibility_enabled");
    const desc = document.getElementById("a11y-desc");
    const btn = document.getElementById("a11y-btn") as HTMLButtonElement;
    if (desc) {
      desc.textContent = enabled
        ? "已开启，App 在后台也能同步"
        : "未开启，App 退到后台将无法同步";
      desc.className = "a11y-desc" + (enabled ? " ok" : "");
    }
    btn.textContent = enabled ? "已开启" : "去开启";
    btn.disabled = enabled;
  } catch {
    /* 忽略 */
  }
}

function initHotkeyInput() {
  const el = $<HTMLInputElement>("#hotkey");
  el.addEventListener("keydown", (e) => {
    e.preventDefault();
    e.stopPropagation();

    if (e.key === "Backspace" || e.key === "Delete") {
      hotkeyRaw = "";
      renderHotkey();
      return;
    }
    const accel = accelFromEvent(e);
    if (accel) {
      hotkeyRaw = accel;
      renderHotkey();
    }
  });
}

interface SyncStatus {
  running: boolean;
  lastError: string | null;
  lastSyncAt: number | null;
  pending: number;
}

interface Entry {
  id: string;
  kind?: "text" | "image";
  time: number;
  dir: "up" | "down";
  preview: string;
  content: string;
  len: number;
  thumb?: string | null;
  usedAt?: number;
}

type Tab = "recent" | "favorites";

const $ = <T extends HTMLElement>(sel: string) => document.querySelector(sel) as T;

let running = false;
let recent: Entry[] = [];
let favorites: Entry[] = [];
let tab: Tab = "recent";
let selected = 0;
let query = "";

// ---------- 设置面板折叠 ----------
function setSettingsOpen(open: boolean) {
  $<HTMLElement>("#settings-card").classList.toggle("hidden", !open);
  $<HTMLButtonElement>("#settings-toggle").classList.toggle("active", open);
  localStorage.setItem("clipsync.settingsOpen", open ? "1" : "0");
}

function initSettingsToggle() {
  const open = localStorage.getItem("clipsync.settingsOpen") === "1";
  setSettingsOpen(open);
  $<HTMLButtonElement>("#settings-toggle").addEventListener("click", () => {
    setSettingsOpen($<HTMLElement>("#settings-card").classList.contains("hidden"));
  });
}

function updateSummary(_cfg: Config) {
  /* 界面上不展示任何 WebDAV 地址信息 */
}

async function loadConfig() {
  try {
    const cfg = await invoke<Config>("get_config");
    $<HTMLInputElement>("#webdav_url").value = cfg.webdavUrl;
    $<HTMLInputElement>("#username").value = cfg.username;
    $<HTMLInputElement>("#password").value = cfg.password;
    $<HTMLSpanElement>("#device_id").textContent = cfg.deviceId;
    hotkeyRaw = cfg.hotkey ?? "";
    renderHotkey();
    updateSummary(cfg);
  } catch (e) {
    console.error(e);
  }
}

async function saveConfig() {
  const url = $<HTMLInputElement>("#webdav_url").value.trim();
  const user = $<HTMLInputElement>("#username").value.trim();
  const pass = $<HTMLInputElement>("#password").value;
  try {
    await invoke("save_config", {
      webdavUrl: url,
      username: user,
      password: pass,
      hotkey: hotkeyRaw,
    });
    updateSummary(await invoke<Config>("get_config"));
    toast("配置已保存");
  } catch (e) {
    toast("保存失败：" + String(e));
  }
}

async function toggle() {
  try {
    if (running) {
      await invoke("stop_sync");
      running = false;
    } else {
      await invoke("start_sync");
      running = true;
    }
    renderToggle();
  } catch (e) {
    toast(String(e));
  }
}

function renderToggle() {
  const btn = $<HTMLButtonElement>("#toggle");
  btn.textContent = running ? "停止同步" : "开始同步";
  btn.classList.toggle("stop", running);
}

function renderStatus(s: SyncStatus) {
  running = s.running;
  renderToggle();
  const state = $<HTMLSpanElement>("#state");
  if (s.running) {
    state.textContent = s.pending > 0 ? `同步中 · 待发送 ${s.pending}` : "同步中";
    state.className = "state-pill ok";
    state.title = s.lastSyncAt
      ? "最近同步 " + new Date(s.lastSyncAt).toLocaleTimeString("zh-CN", { hour12: false })
      : "同步中";
  } else {
    state.textContent = "未运行";
    state.className = "state-pill";
    state.title = "";
  }
  $<HTMLDivElement>("#error").textContent = s.lastError ?? "";
}

// ---------- 时间显示 ----------
function relativeTime(ts: number): string {
  const now = Date.now();
  const diff = Math.max(0, now - ts);
  const d = new Date(ts);
  const hhmm = d.toLocaleTimeString("zh-CN", {
    hour: "2-digit",
    minute: "2-digit",
    hour12: false,
  });

  if (diff < 60_000) return "刚刚";
  if (diff < 60 * 60_000) return Math.floor(diff / 60_000) + " 分钟前";

  const today = new Date();
  const sameDay = (a: Date, b: Date) =>
    a.getFullYear() === b.getFullYear() &&
    a.getMonth() === b.getMonth() &&
    a.getDate() === b.getDate();

  if (sameDay(d, today)) return "今天 " + hhmm;

  const yesterday = new Date(today);
  yesterday.setDate(today.getDate() - 1);
  if (sameDay(d, yesterday)) return "昨天 " + hhmm;

  const mmdd = `${String(d.getMonth() + 1).padStart(2, "0")}-${String(
    d.getDate()
  ).padStart(2, "0")}`;
  if (d.getFullYear() === today.getFullYear()) return `${mmdd} ${hhmm}`;
  return `${d.getFullYear()}-${mmdd} ${hhmm}`;
}

// ---------- 记录列表 ----------
function escapeHtml(s: string): string {
  return s
    .replace(/&/g, "&amp;")
    .replace(/</g, "&lt;")
    .replace(/>/g, "&gt;")
    .replace(/"/g, "&quot;");
}

async function refreshHistory() {
  try {
    const [r, f] = await Promise.all([
      invoke<Entry[]>("get_history"),
      invoke<Entry[]>("get_favorites"),
    ]);
    recent = r;
    favorites = f;
    renderHistory();
  } catch (e) {
    console.error(e);
  }
}

function setTab(next: Tab) {
  tab = next;
  selected = 0;
  document.querySelectorAll<HTMLButtonElement>(".hist-tab").forEach((b) => {
    b.classList.toggle("active", b.dataset.tab === next);
  });
  // 「清空」只对最近记录生效
  $<HTMLButtonElement>("#clear-history").style.visibility =
    next === "recent" ? "visible" : "hidden";
  renderHistory();
}

function renderHistory() {
  $<HTMLSpanElement>("#count-recent").textContent = String(recent.length);
  $<HTMLSpanElement>("#count-fav").textContent = String(favorites.length);

  const list = $<HTMLDivElement>("#history-list");
  const all = tab === "recent" ? recent : favorites;
  const q = query.trim().toLowerCase();
  const items = q
    ? all.filter(
        (e) =>
          e.preview.toLowerCase().includes(q) ||
          (e.kind !== "image" && e.content.toLowerCase().includes(q))
      )
    : all;

  if (!items.length) {
    selected = 0;
    list.innerHTML = q
      ? '<div class="history-empty">没有匹配的记录</div>'
      : tab === "recent"
      ? '<div class="history-empty">最近 7 天暂无同步记录</div>'
      : '<div class="history-empty">还没有收藏 · 点记录右侧的 ☆ 收藏</div>';
    return;
  }

  const favIds = new Set(favorites.map((e) => e.id));

  if (selected >= items.length) selected = items.length - 1;
  if (selected < 0) selected = 0;

  list.innerHTML = "";
  items.forEach((it, idx) => {
    const isUp = it.dir === "up";
    const isFav = favIds.has(it.id);
    const row = document.createElement("div");
    row.className = "history-row" + (idx === selected ? " selected" : "");
    row.dataset.index = String(idx);
    row.title = "点击复制这条内容";

    const arrow = document.createElement("div");
    arrow.className = "history-arrow " + (isUp ? "up" : "down");
    arrow.textContent = isUp ? "↑" : "↓";

    const body = document.createElement("div");
    body.className = "history-body";

    const isImage = it.kind === "image";
    const meta = document.createElement("div");
    meta.className = "history-meta";
    meta.innerHTML = `
      <span class="type-badge ${isImage ? "img" : ""}">${isImage ? "图片" : "纯文本"}</span>
      ${isImage ? "" : `<span class="meta-text">${it.len} 个字符</span>`}
      <span class="meta-dot">·</span>
      <span class="meta-text">${relativeTime(Math.max(it.time, it.usedAt || 0))}</span>
      ${it.usedAt ? '<span class="meta-text used-tag">已使用</span>' : ""}
      <span class="meta-text dir-label ${isUp ? "up" : "down"}">${
      isUp ? "发送到云端" : "从云端接收"
    }</span>`;

    const content = document.createElement("div");
    content.className = "history-content";
    content.textContent = it.preview;

    body.appendChild(meta);
    body.appendChild(content);

    if (isImage && it.thumb) {
      const img = document.createElement("img");
      img.className = "thumb";
      img.src = it.thumb;
      img.alt = it.preview;
      body.appendChild(img);
    }

    const actions = document.createElement("div");
    actions.className = "row-actions";

    const favBtn = document.createElement("button");
    favBtn.className = "icon-mini fav-btn" + (isFav ? " on" : "");
    if (isImage) favBtn.style.display = "none";
    favBtn.title = isFav ? "取消收藏" : "收藏（永久保留）";
    favBtn.textContent = isFav ? "★" : "☆";
    favBtn.addEventListener("click", async (e) => {
      e.stopPropagation();
      try {
        await invoke<boolean>("toggle_favorite", { id: it.id });
        await refreshHistory();
      } catch (err) {
        toast(String(err));
      }
    });

    // ---- 内容类型快捷按钮 ----
    const detected = isImage ? { kind: null as DetectedKind } : detectContent(it.content || it.preview);
    const quickBtns: HTMLElement[] = [];

    if (detected.kind === "color") {
      const sw = document.createElement("span");
      sw.className = "color-swatch";
      const c = (it.content || it.preview).trim();
      sw.style.background = c;
      sw.title = `${c}（点击查看）`;
      sw.addEventListener("click", async (e) => {
        e.stopPropagation();
        const rgb = hexToRgb(c);
        const dlg = $<HTMLDialogElement>("#json-dlg");
        $<HTMLTextAreaElement>("#json-text").value = rgb
          ? `${c}\nrgb(${rgb.r}, ${rgb.g}, ${rgb.b})\nrgba(${rgb.r}, ${rgb.g}, ${rgb.b}, 1)`
          : c;
        jsonStatus("颜色值");
        dlg.showModal();
        await touchEntry(it.id);
      });
      quickBtns.push(sw);
    }

    if (detected.kind === "url") {
      const b = document.createElement("button");
      b.className = "icon-mini quick-btn";
      b.textContent = "↗";
      b.title = "在浏览器打开";
      b.addEventListener("click", async (e) => {
        e.stopPropagation();
        try {
          await invoke("open_url", { url: (it.content || it.preview).trim() });
          await touchEntry(it.id);
        } catch (err) {
          toast(String(err));
        }
      });
      quickBtns.push(b);
    }

    if (detected.kind === "jwt" || detected.kind === "base64" || detected.kind === "timestamp") {
      const b = document.createElement("button");
      b.className = "icon-mini quick-btn";
      b.textContent = detected.kind === "jwt" ? "JWT" : detected.kind === "base64" ? "B64" : "⏱";
      b.title =
        detected.kind === "jwt" ? "解码 JWT" : detected.kind === "base64" ? "解码 Base64" : "时间戳转日期";
      b.addEventListener("click", (e) => {
        e.stopPropagation();
        const raw = (it.content || it.preview).trim();
        const dlg = $<HTMLDialogElement>("#json-dlg");
        if (detected.kind === "jwt") {
          $<HTMLTextAreaElement>("#json-text").value = decodedJwt(raw);
          jsonStatus("JWT 已解码");
        } else if (detected.kind === "base64") {
          $<HTMLTextAreaElement>("#json-text").value = tryDecodeBase64(raw) ?? raw;
          jsonStatus("Base64 已解码");
        } else {
          const ms = raw.length === 10 ? Number(raw) * 1000 : Number(raw);
          const d = new Date(ms);
          $<HTMLTextAreaElement>("#json-text").value =
            `时间戳: ${raw}\n本地时间: ${d.toLocaleString("zh-CN")}\nISO: ${d.toISOString()}`;
          jsonStatus("时间戳");
        }
        dlg.showModal();
      });
      quickBtns.push(b);
    }

    const jsonBtn = document.createElement("button");
    jsonBtn.className = "icon-mini json-btn";
    jsonBtn.title = "JSON 工具";
    jsonBtn.textContent = "{}";
    jsonBtn.style.display = isImage ? "none" : "";
    jsonBtn.addEventListener("click", (e) => {
      e.stopPropagation();
      openJsonTools(it.content || it.preview);
    });

    const copyBtn = document.createElement("button");
    copyBtn.className = "icon-mini";
    copyBtn.title = isImage ? (IS_ANDROID ? "保存到相册" : "复制图片") : "复制";
    copyBtn.innerHTML =
      '<svg viewBox="0 0 24 24" width="15" height="15" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round"><rect x="9" y="9" width="11" height="11" rx="2"/><path d="M5 15V5a2 2 0 0 1 2-2h8"/></svg>';
    copyBtn.addEventListener("click", async (e) => {
      e.stopPropagation();
      await copyEntry(it);
    });

    actions.appendChild(favBtn);
    const quickWrap = document.createElement("div");
    quickWrap.className = "quick-wrap";
    quickBtns.forEach((b) => quickWrap.appendChild(b));
    if (quickBtns.length) actions.appendChild(quickWrap);
    actions.appendChild(jsonBtn);
    actions.appendChild(copyBtn);

    row.addEventListener("click", () => {
      if (isImage) {
        void previewImage(it);
      } else {
        void copyEntry(it);
      }
    });

    row.appendChild(arrow);
    row.appendChild(body);
    row.appendChild(actions);
    list.appendChild(row);
  });
}

const IS_ANDROID = /Android/i.test(navigator.userAgent);

// ---------- 内容类型识别 ----------
type DetectedKind = "url" | "color" | "jwt" | "base64" | "timestamp" | "email" | null;

interface Detected {
  kind: DetectedKind;
  label?: string;
}

function tryDecodeBase64(text: string): string | null {
  try {
    const cleaned = text.replace(/\s/g, "");
    if (cleaned.length < 8) return null;
    const bin = atob(cleaned);
    const bytes = Uint8Array.from(bin, (c) => c.charCodeAt(0));
    const txt = new TextDecoder("utf-8", { fatal: false }).decode(bytes);
    // 可打印字符占比过低就不认为是文本
    const printable = [...txt].filter((c) => c >= " " || c === "\n" || c === "\t").length;
    return printable / Math.max(1, txt.length) > 0.9 ? txt : null;
  } catch {
    return null;
  }
}

function detectContent(raw: string): Detected {
  const t = raw.trim();
  if (!t || t.length > 20000) return { kind: null };

  if (/^https?:\/\/[^\s]+$/i.test(t)) return { kind: "url" };
  if (/^#([0-9a-f]{3}|[0-9a-f]{6}|[0-9a-f]{8})$/i.test(t)) return { kind: "color" };
  if (/^rgba?\(\s*\d+\s*,\s*\d+\s*,\s*\d+/i.test(t)) return { kind: "color" };
  if (/^[^\s@]+@[^\s@]+\.[^\s@]+$/.test(t) && t.length < 120) return { kind: "email" };

  const jwt = /^[A-Za-z0-9_-]{8,}\.[A-Za-z0-9_-]{8,}\.[A-Za-z0-9_-]*$/;
  if (jwt.test(t)) return { kind: "jwt" };

  if (/^\d{10}$/.test(t) || /^\d{13}$/.test(t)) {
    const ms = t.length === 10 ? Number(t) * 1000 : Number(t);
    const d = new Date(ms);
    if (d.getFullYear() > 1990 && d.getFullYear() < 2200) return { kind: "timestamp" };
  }

  if (/^[A-Za-z0-9+/=\s]{16,}$/.test(t) && (t.length % 4 === 0 || t.includes("="))) {
    if (tryDecodeBase64(t) !== null) return { kind: "base64" };
  }
  return { kind: null };
}

function decodedJwt(text: string): string {
  const parts = text.trim().split(".");
  const dec = (seg: string) => {
    try {
      const b64 = seg.replace(/-/g, "+").replace(/_/g, "/");
      const pad = b64 + "=".repeat((4 - (b64.length % 4)) % 4);
      return JSON.stringify(JSON.parse(tryDecodeBase64(pad) || ""), null, 2);
    } catch {
      return "(无法解析)";
    }
  };
  return `// header\n${dec(parts[0])}\n\n// payload\n${dec(parts[1])}`;
}

// ---------- JSON 工具 ----------
function escapeJsonString(text: string): string {
  return JSON.stringify(text).slice(1, -1);
}

function unescapeJsonString(text: string): string {
  const t = text.trim();
  try {
    if (t.startsWith('"') && t.endsWith('"')) return JSON.parse(t);
  } catch {
    /* 继续尝试 */
  }
  try {
    return JSON.parse('"' + t + '"');
  } catch {
    /* 退回简单替换 */
  }
  return t
    .replace(/\\r/g, "\r")
    .replace(/\\n/g, "\n")
    .replace(/\\t/g, "\t")
    .replace(/\\"/g, '"')
    .replace(/\\\\/g, "\\");
}

function hexToRgb(hex: string): { r: number; g: number; b: number } | null {
  const m = hex.trim().match(/^#([0-9a-f]{3}|[0-9a-f]{6}|[0-9a-f]{8})$/i);
  if (!m) return null;
  let h = m[1];
  if (h.length === 3) h = h.split("").map((c) => c + c).join("");
  if (h.length === 8) h = h.slice(0, 6);
  return {
    r: parseInt(h.slice(0, 2), 16),
    g: parseInt(h.slice(2, 4), 16),
    b: parseInt(h.slice(4, 6), 16),
  };
}

function jsonStatus(msg: string, ok = true) {
  const el = $<HTMLDivElement>("#json-status");
  el.textContent = msg;
  el.className = "json-status" + (ok ? "" : " err");
}

function openJsonTools(text: string) {
  const dlg = $<HTMLDialogElement>("#json-dlg");
  $<HTMLTextAreaElement>("#json-text").value = text;
  jsonStatus("");
  dlg.showModal();
}

function initJsonTools() {
  const dlg = $<HTMLDialogElement>("#json-dlg");
  const area = $<HTMLTextAreaElement>("#json-text");

  $<HTMLButtonElement>("#json-close").addEventListener("click", () => dlg.close());

  $<HTMLButtonElement>("#image-close").addEventListener("click", () =>
    $<HTMLDialogElement>("#image-dlg").close()
  );

  $<HTMLButtonElement>("#json-format").addEventListener("click", () => {
    try {
      area.value = JSON.stringify(JSON.parse(area.value), null, 2);
      jsonStatus("已格式化");
    } catch (e) {
      jsonStatus("不是合法 JSON：" + String(e).slice(0, 80), false);
    }
  });

  $<HTMLButtonElement>("#json-minify").addEventListener("click", () => {
    try {
      area.value = JSON.stringify(JSON.parse(area.value));
      jsonStatus("已压缩为单行");
    } catch (e) {
      jsonStatus("不是合法 JSON：" + String(e).slice(0, 80), false);
    }
  });

  $<HTMLButtonElement>("#json-escape").addEventListener("click", () => {
    area.value = escapeJsonString(area.value);
    jsonStatus("已转义");
  });

  $<HTMLButtonElement>("#json-unescape").addEventListener("click", () => {
    area.value = unescapeJsonString(area.value);
    jsonStatus("已去转义");
  });

  $<HTMLButtonElement>("#json-copy").addEventListener("click", async () => {
    await copyToClipboard(area.value);
  });
}

/** 记录被使用后挪到最前（并保持选中这一条） */
async function touchEntry(id: string) {
  try {
    await invoke("touch_entry", { id });
    await refreshHistory();
    const items = currentItems();
    const idx = items.findIndex((e) => e.id === id);
    if (idx >= 0) {
      selected = idx;
      renderHistory();
      document
        .querySelector<HTMLElement>(`.history-row[data-index="${selected}"]`)
        ?.scrollIntoView({ block: "nearest" });
    }
  } catch (e) {
    console.error(e);
  }
}

async function importSharedImage() {
  try {
    const msg = await invoke<string>("import_shared_image");
    if (msg) {
      toast(msg);
      await refreshHistory();
    }
  } catch (e) {
    console.error(e);
  }
}

async function previewImage(it: Entry) {
  const dlg = $<HTMLDialogElement>("#image-dlg");
  const img = $<HTMLImageElement>("#image-preview");
  $<HTMLHeadingElement>("#image-title").textContent = "图片预览";
  $<HTMLSpanElement>("#image-meta").textContent = "加载中…";
  dlg.showModal();

  // 先用缩略图顶一下，避免空白
  if (it.thumb) img.src = it.thumb;

  try {
    const dataUrl = await invoke<string>("image_data_url", { hash: it.content });
    img.src = dataUrl;
    $<HTMLSpanElement>("#image-meta").textContent = it.preview;
  } catch (e) {
    $<HTMLSpanElement>("#image-meta").textContent = String(e);
  }

  $<HTMLButtonElement>("#image-copy").onclick = async () => {
    const msg = await invoke<string>("copy_image", { hash: it.content }).catch((err) => String(err));
    toast(msg);
    await touchEntry(it.id);
  };
}

async function copyEntry(it: Entry) {
  if (it.kind === "image") {
    try {
      const msg = await invoke<string>("copy_image", { hash: it.content });
      toast(msg || "已保存");
      await touchEntry(it.id);
    } catch (e) {
      toast(String(e));
    }
    return;
  }
  await copyToClipboard(it.content || it.preview);
  await touchEntry(it.id);
}

async function copyToClipboard(text: string) {
  if (!text) {
    toast("图片记录暂不支持复制到剪贴板");
    return;
  }
  try {
    await navigator.clipboard.writeText(text);
    toast("已复制到剪贴板");
  } catch {
    toast("复制失败");
  }
}

// ---------- 键盘操作 ----------
function currentItems(): Entry[] {
  const all = tab === "recent" ? recent : favorites;
  const q = query.trim().toLowerCase();
  if (!q) return all;
  return all.filter(
    (e) =>
      e.preview.toLowerCase().includes(q) ||
      (e.kind !== "image" && e.content.toLowerCase().includes(q))
  );
}

function moveSelection(delta: number) {
  const items = currentItems();
  if (!items.length) return;
  selected = Math.min(items.length - 1, Math.max(0, selected + delta));
  renderHistory();
  const el = document.querySelector<HTMLElement>(
    `.history-row[data-index="${selected}"]`
  );
  el?.scrollIntoView({ block: "nearest" });
}

async function pasteSelected() {
  const items = currentItems();
  const it = items[selected];
  if (!it) {
    toast("没有可粘贴的记录");
    return;
  }
  if (IS_ANDROID) {
    toast("手机端请点击记录复制");
    return;
  }
  try {
    await invoke("paste_entry", { id: it.id });
    await touchEntry(it.id);
  } catch (e) {
    toast(String(e));
  }
}

function initKeyboard() {
  document.addEventListener("keydown", (e) => {
    const target = e.target as HTMLElement | null;
    // 输入框内不拦截方向键，避免影响编辑
    if (target && (target.tagName === "INPUT" || target.tagName === "TEXTAREA")) {
      return;
    }

    switch (e.key) {
      case "ArrowUp":
        e.preventDefault();
        moveSelection(-1);
        break;
      case "ArrowDown":
        e.preventDefault();
        moveSelection(1);
        break;
      case "ArrowLeft":
        e.preventDefault();
        setTab("recent");
        break;
      case "ArrowRight":
        e.preventDefault();
        setTab("favorites");
        break;
      case "Enter":
        e.preventDefault();
        void pasteSelected();
        break;
      case "Escape":
        e.preventDefault();
        void getCurrentWindow().hide();
        break;
      default:
        break;
    }
  });
}

function toast(msg: string) {
  const t = $<HTMLDivElement>("#toast");
  t.textContent = msg;
  t.classList.add("show");
  setTimeout(() => t.classList.remove("show"), 2600);
}

async function init() {
  initSettingsToggle();
  initHotkeyInput();
  initKeyboard();
  initJsonTools();
  void refreshA11y();

  document.getElementById("a11y-btn")?.addEventListener("click", async () => {
    await invoke("open_accessibility_settings");
    toast("请在弹出的设置里找到 ClipSync 并开启");
    setTimeout(() => void refreshA11y(), 2500);
  });
  await loadConfig();
  try {
    renderStatus(await invoke<SyncStatus>("get_status"));
  } catch (e) {
    console.error(e);
  }
  await refreshHistory();
  setTab("recent");

  // 手机端：处理「分享到 ClipSync」进来的图片
  await importSharedImage();
  // 分享进来时窗口焦点可能不变，这里定时兜底检查
  if (IS_ANDROID) {
    setInterval(() => void importSharedImage(), 2500);
  }
  try {
    await getCurrentWindow().onFocusChanged(({ payload }) => {
      if (payload) {
        void importSharedImage();
        void refreshA11y();
      }
    });
  } catch {
    /* 某些平台不支持焦点事件 */
  }

  await listen<SyncStatus>("sync-status", (event) => renderStatus(event.payload));
  await listen("sync-history", () => refreshHistory());

  // 相对时间每分钟刷新一次
  setInterval(() => {
    if (recent.length || favorites.length) renderHistory();
  }, 60_000);

  document.querySelectorAll<HTMLButtonElement>(".hist-tab").forEach((b) => {
    b.addEventListener("click", () => setTab((b.dataset.tab as Tab) ?? "recent"));
  });

  const searchEl = $<HTMLInputElement>("#search");
  searchEl.addEventListener("input", () => {
    query = searchEl.value;
    selected = 0;
    renderHistory();
  });
  // 输入框内按 Esc 清空搜索
  searchEl.addEventListener("keydown", (e) => {
    if (e.key === "Escape") {
      e.stopPropagation();
      searchEl.value = "";
      query = "";
      selected = 0;
      renderHistory();
      searchEl.blur();
    }
  });

  $<HTMLButtonElement>("#save").addEventListener("click", saveConfig);
  $<HTMLButtonElement>("#toggle").addEventListener("click", toggle);
  // 一键清理图片（Android 清相册目录 / 桌面清本地缓存）
  {
    const btn = $<HTMLButtonElement>("#clear-images");
    btn.textContent = IS_ANDROID ? "清理相册" : "清理缓存";
    btn.addEventListener("click", async () => {
      const what = IS_ANDROID
        ? "确定清理相册里 ClipSync 保存的所有图片吗？"
        : "确定清理本地图片缓存吗？";
      if (!confirm(what)) return;
      try {
        const msg = await invoke<string>("clear_image_storage");
        toast(msg);
        await refreshHistory();
      } catch (e) {
        toast(String(e));
      }
    });
  }

  $<HTMLButtonElement>("#clear-history").addEventListener("click", async () => {
    await invoke("clear_history");
    await refreshHistory();
    toast("最近记录已清空（收藏不受影响）");
  });
}

init().catch((e) => console.error(e));
