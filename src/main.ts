import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { open } from "@tauri-apps/plugin-dialog";

let installMsgEl: HTMLElement;

let createPatchProgressEl: HTMLProgressElement;
let createPatchMsgEl: HTMLElement;
let createPatchPathMsgEl: HTMLElement;

async function install() {
  if (installMsgEl) {
    installMsgEl.textContent = await invoke("install");
  }
}

window.addEventListener("DOMContentLoaded", () => {
  installMsgEl = document.querySelector("#install-msg") ?? throwNull();
  createPatchProgressEl = document.querySelector("#create-patch-progress") ?? throwNull();
  createPatchMsgEl = document.querySelector("#create-patch-msg") ?? throwNull();
  createPatchPathMsgEl = document.querySelector("#create-patch-path-msg") ?? throwNull();

  document.querySelector("#install-form")?.addEventListener("submit", (e) => {
    e.preventDefault();
    install();
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

listen<string>("install-finished", (event) => {
  console.log(`downloading ${event.payload}`);
});

listen<CreatePatchProgress>("create-patch-progress", (event) => {
  let payload = event.payload;

  createPatchProgressEl.value = payload.done_files;
  createPatchProgressEl.max = payload.total_files;
  createPatchMsgEl.textContent = `${payload.done_files} / ${payload.total_files}`;
  createPatchPathMsgEl.textContent = `${payload.path}`;
});

function throwNull(): never {
  throw new Error();
}