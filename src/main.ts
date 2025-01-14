import { getVersion } from '@tauri-apps/api/app';;
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { open } from "@tauri-apps/plugin-dialog";
import { toReadableSize } from "./util";

let updateCheckFinished = false;

let installMsgEl: HTMLElement;
let installNetIoSpanEl: HTMLSpanElement;
let installNetProgressEl: HTMLProgressElement;
let installDiskIoSpanEl: HTMLSpanElement;
let installDiskProgressEl: HTMLProgressElement;

let createPatchProgressEl: HTMLProgressElement;
let createPatchMsgEl: HTMLElement;
let createPatchPathMsgEl: HTMLElement;

let installForm: HTMLFormElement;
let versionSpan: HTMLSpanElement;

window.addEventListener("DOMContentLoaded", () => {
  installMsgEl = document.querySelector("#install-msg") ?? throwNull();
  installNetIoSpanEl = document.querySelector("#install-net-io-span") ?? throwNull();
  installNetProgressEl = document.querySelector("#install-net-progress") ?? throwNull();
  installDiskIoSpanEl = document.querySelector("#install-disk-io-span") ?? throwNull();
  installDiskProgressEl = document.querySelector("#install-disk-progress") ?? throwNull();

  createPatchProgressEl = document.querySelector("#create-patch-progress") ?? throwNull();
  createPatchMsgEl = document.querySelector("#create-patch-msg") ?? throwNull();
  createPatchPathMsgEl = document.querySelector("#create-patch-path-msg") ?? throwNull();

  installForm = document.querySelector<HTMLFormElement>("#install-form") ?? throwNull();
  versionSpan = document.querySelector<HTMLSpanElement>("#patcher-version-span") ?? throwNull();

  getVersion().then((version) => {
    versionSpan.textContent = version;
  });
  
  invoke<[boolean, string]>("get_update_check_status").then((value) => {
    if (value[0]) {
      enableElementsOnReady();
    }
    versionSpan.title = value[1];
  });

  installForm.addEventListener("submit", async (e) => {
    e.preventDefault();

    if (e.submitter instanceof HTMLButtonElement) {
      e.submitter.disabled = true;
    }
    installNetProgressEl.classList.remove("progress-error");
    installDiskProgressEl.classList.remove("progress-error");
    installNetProgressEl.value = 0;
    installDiskProgressEl.value = 0;

    try {
      await invoke("install");
      installMsgEl.textContent = `Installation finished`;
    }
    catch (err) {
      installMsgEl.textContent = `Error: ${err}`;
      installNetProgressEl.classList.add("progress-error");
      installDiskProgressEl.classList.add("progress-error");
    }

    if (e.submitter instanceof HTMLButtonElement) {
      e.submitter.disabled = false;
    }
  });

  let browseButtons = document.querySelectorAll<HTMLButtonElement>("#create-patch-form button[x\\:for]");
  for (let button of browseButtons) {
    let inputName = button.attributes.getNamedItem("x:for")?.value;
    let input = document.querySelector<HTMLInputElement>(`#create-patch-form input[name='${inputName}']`);
    if (!input) {
      continue;
    }

    let inputType = button.attributes.getNamedItem("x:type")?.value;
    button.addEventListener("click", async (ev) => {
      ev.preventDefault();
      let path = await open({
        directory: inputType == "folder"
      });
      if (path) {
        input.value = path;
      }
    });
  }

  let form = document.querySelector<HTMLFormElement>("#create-patch-form");
  form?.addEventListener("submit", async (e) => {
    e.preventDefault();

    var data = new FormData(form);
    let outDir = data.get("out")?.toString();
    let newDir = data.get("new")?.toString();
    let oldDir = data.get("old")?.toString();
    if (outDir && newDir) {
      if (e.submitter instanceof HTMLButtonElement) {
        e.submitter.disabled = true;
      }

      try {
        let result = await invoke<CreatePatchResult>("create_patch", { outDir, newDir, oldDir });
        let manifest = result.manifest;

        let newCount = manifest.new_files.length;
        let diffCount = manifest.diff_files.length;
        let staleCount = manifest.stale_files.length;
        let totalCount = newCount + diffCount;
        createPatchProgressEl.value = totalCount;

        let patchSizeMB = result.patch_size / (1024.0 * 1024.0);
        let fractionDigits = patchSizeMB >= 1000 ? 0 : 1;
        let sizeStr = patchSizeMB.toFixed(fractionDigits) + "MiB";
        createPatchMsgEl.textContent =
          `Created ${sizeStr} patch with ${totalCount} files ` +
          `(${newCount} new, ${diffCount} diff, ${staleCount} stale)`;
      } catch (err) {
        createPatchProgressEl.value = 0;
        createPatchMsgEl.textContent = `Error: ${err}`;
      }

      createPatchPathMsgEl.textContent = "";
      if (e.submitter instanceof HTMLButtonElement) {
        e.submitter.disabled = false;
      }
    }
  });
});

type CreatePatchProgress = {
  done_files: number;
  total_files: number;
  path: string;
};

type InstallProgress = {
  net: ProgressState;
  disk: ProgressState;
  message: string;
};

type ProgressState = {
  value: number;
  max: number;
  known: boolean;
};

listen<InstallProgress>("install-progress", (event) => {
  let payload = event.payload;

  updateProgress(installNetIoSpanEl, installNetProgressEl, payload.net);
  updateProgress(installDiskIoSpanEl, installDiskProgressEl, payload.disk);
  installMsgEl.textContent = payload.message;
});

function updateProgress(span: HTMLSpanElement, bar: HTMLProgressElement, state: ProgressState) {
  span.textContent = `${toReadableSize(state.value, 2)} / ${toReadableSize(state.max, 2)}`;

  if (state.known) {
    bar.value = state.value;
  } else {
    bar.removeAttribute("value");
  }
  bar.max = state.max;
}

type CreatePatchResult = {
  manifest: PatchManifest;
  patch_size: number;
};

type FileManifest = {
  path: string;
  len: number;
  hash: Uint8Array,
};

type PatchManifest = {
  manifest_version: string,
  new_files: string[],
  diff_files: FileManifest[],
  stale_files: string[],
}

listen<CreatePatchProgress>("create-patch-progress", (event) => {
  let payload = event.payload;

  createPatchProgressEl.value = payload.done_files;
  createPatchProgressEl.max = payload.total_files;

  createPatchMsgEl.textContent = `${payload.done_files} / ${payload.total_files}`;
  createPatchPathMsgEl.textContent = `${payload.path}`;
});

listen<[boolean, string]>("update-check-finished", (event) => {
  console.log("update check finished: ", event);

  if (document.readyState == "complete") {
    enableElementsOnReady();
    versionSpan.title = event.payload[1];
  }
});

function throwNull(): never {
  throw new Error();
}

function enableElementsOnReady() {
  if (updateCheckFinished) {
    return;
  }
  updateCheckFinished = true;

  for (let button of installForm.querySelectorAll("button")) {
    button.disabled = false;
  }
}