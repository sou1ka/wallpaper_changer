const { invoke, convertFileSrc } = window.__TAURI__.tauri;
const { open } = window.__TAURI__.dialog;
const { appWindow } = window.__TAURI__.window;

async function saveConfig() {
  const interval = document.querySelector('input[name="interval"]').value;
  const startTime = document.querySelector('input[name="startTime"]').value;
  const endTime = document.querySelector('input[name="endTime"]').value;

  const week = document.querySelector('select[name="week"]').value;
  const day = document.querySelector('select[name="day"]').value;

  // weekly は複数選択にする予定なら配列化するが、
  // 現状は単一選択なので空文字は null 扱いにする
  const weekly = week ? [week] : null;

  // monthly も単一選択 → 配列化
  const monthly = day ? [Number(day)] : null;

  const payload = {
    interval: Number(interval),
    startDt: startTime || null,
    endDt: endTime || null,
    weekly: week ? [week] : null,
    monthly: day ? [Number(day)] : null,
    defaultWallpaperPath: null, // 必要なら設定
    fileTargets: [] // 必要なら設定
  };

  //console.log("Saving config:", payload);
  await invoke("save_config", { config: payload });
}

function setupAutoSave() {
  const inputs = document.querySelectorAll(
    'input[name="interval"], input[name="startTime"], input[name="endTime"], select[name="week"], select[name="day"]'
  );

  inputs.forEach((el) => {
    el.addEventListener("input", saveConfig);
    el.addEventListener("change", saveConfig);
  });
}

async function loadConfig() {
  const cfg = await invoke("load_config_for_frontend");
  //console.log("Loaded config:", cfg);

  document.querySelector('input[name="interval"]').value = cfg.interval ?? 60;
  document.querySelector('input[name="startTime"]').value = cfg.startDt ?? "";
  document.querySelector('input[name="endTime"]').value = cfg.endDt ?? "";

  // weekly は配列なので先頭だけ反映（UI が単一選択のため）
  document.querySelector('select[name="week"]').value =
    cfg.weekly && cfg.weekly.length > 0 ? cfg.weekly[0] : "";

  // monthly も配列 → 先頭だけ反映
  document.querySelector('select[name="day"]').value =
    cfg.monthly && cfg.monthly.length > 0 ? String(cfg.monthly[0]) : "";

      // --- サムネイル表示 ---
  const container = document.querySelector(".imagelist");

  // 初期化
  container.innerHTML = "";

  if (!cfg.fileTargets || cfg.fileTargets.length === 0) {
    container.textContent = "Not selected...";
    return;
  }

  renderThumbnails(cfg.fileTargets);
}

// ページロード時に呼ぶ
window.addEventListener("DOMContentLoaded", function() {
    loadConfig();
    relImage();
    initDD();
    setupAutoSave();
});

document.addEventListener('contextmenu', function(e) {
  e.preventDefault();
  e.stopPropagation();
}, false);

document.addEventListener('selectstart', function(e) {
  e.preventDefault();
  e.stopPropagation();
});

document.addEventListener('keydown', async function(e) {
  if(e.key == 'F5' || (e.ctrlKey && e.key == 'r') || e.key == 'F7') {
    e.preventDefault();
    e.stopPropagation();
  }
});

// ファイル参照
async function relImage() {
    document.querySelector(".app-btn").addEventListener("click", async () => {
    const selected = await open({
        multiple: true,
        directory: false,
        recursive: true,
        filters: [
          { name: "Images", extensions: ["jpg", "jpeg", "png", "bmp", "gif", "webp"] }
        ]
    });

    if (!selected) { return };

    const paths = Array.isArray(selected) ? selected : [selected];
    addImage(paths);
  });
}

function initDD() {
  const dropArea = document.querySelector(".imagelist");

  // 見た目用の dragover はそのまま（ブラウザ内 D&D 用）
  dropArea.addEventListener("dragover", (e) => {
    e.preventDefault();
    dropArea.classList.add("dragover");
  });

  dropArea.addEventListener("dragleave", (e) => {
    e.preventDefault();
    dropArea.classList.remove("dragover");
  });

  appWindow.onFileDropEvent(async (event) => {
    const payload = event.payload;

    if (payload.type === "hover") {
      dropArea.classList.add("dragover");
      return;
    }

    if (payload.type === "cancel") {
      dropArea.classList.remove("dragover");
      return;
    }

    if (payload.type === "drop") {
      dropArea.classList.remove("dragover");
      const paths = payload.paths; // ここにファイル/フォルダのパス配列が入る

      if (!paths || paths.length === 0) { return; }

      addImage(paths);
    }
  });
}

async function renderThumbnails(paths) {
  const container = document.querySelector(".imagelist");
  container.innerHTML = "";

  for (const path of paths) {
    appendThumbnail(path);
  }
}

async function removeThumbnail(img, path) {
  // フェードアウト開始
  img.classList.add("fade-out");

  // アニメーション終了後に削除
  img.addEventListener("transitionend", async () => {
    img.remove();

    // Rust 側の fileTargets を更新
    path = path.replace(/\//g, "\\");
    const updated = await invoke("remove_file_target", { path });
  }, { once: true });
}

async function addImage(paths) {
  const updated = await invoke("add_file_targets", { paths });
  //renderThumbnails(updated);
  for (let p of updated) {
    p = p.replace(/\\/g, '/');
    if (!document.querySelector(`img[data-path="${p}"]`)) {
      appendThumbnail(p);
    }
  }
}

async function appendThumbnail(path) {
  const img = document.createElement("img");
  img.src = convertFileSrc(path);
  img.width = 120;
  img.style.margin = "4px";
  img.dataset.path = path.replace(/\\/g, '/');
  img.classList.add("fade-in");

  const container = document.querySelector(".imagelist");
  container.appendChild(img);

  requestAnimationFrame(() => {
    img.classList.remove("fade-in");
  });

  img.addEventListener("dblclick", () => removeThumbnail(img, path));
}
