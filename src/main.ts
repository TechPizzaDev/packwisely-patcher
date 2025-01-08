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

async function createPatch(outDir: string, newDir: string, oldDir: string | undefined) {
  let result = await invoke<CreatePatchResult>("create_patch", { outDir, newDir, oldDir });

  createPatchProgressEl.value = result.totalFiles;
  createPatchMsgEl.textContent = `Created patch with ${result.totalFiles} files`;
  createPatchPathMsgEl.textContent = "";
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
  form?.addEventListener("submit", (e) => {
    e.preventDefault();
    
    var data = new FormData(form);
    let outDir = data.get("out")?.toString();
    let newDir = data.get("new")?.toString();
    let oldDir = data.get("old")?.toString();
    if (outDir && newDir) {
      createPatch(outDir, newDir, oldDir);
    }
  });
});

type CreatePatchProgress = {
  doneFiles: number;
  totalFiles: number;
  path: string;
};

type CreatePatchResult = {
  totalFiles: number;
};

listen<string>("install-finished", (event) => {
  console.log(`downloading ${event.payload}`);
});

listen<CreatePatchProgress>("create-patch-progress", (event) => {
  let payload = event.payload;

  createPatchProgressEl.value = payload.doneFiles;
  createPatchProgressEl.max = payload.totalFiles;
  createPatchMsgEl.textContent = `${payload.doneFiles} / ${payload.totalFiles}`;
  createPatchPathMsgEl.textContent = `${payload.path}`;
});

function throwNull(): never {
  throw new Error();
}