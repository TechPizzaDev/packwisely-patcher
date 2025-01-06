import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";

let installMsgEl: HTMLElement | null;

async function install() {
  if (installMsgEl) {
    installMsgEl.textContent = await invoke("install");
  }
}

window.addEventListener("DOMContentLoaded", () => {
  installMsgEl = document.querySelector("#install-msg");

  document.querySelector("#install-form")?.addEventListener("submit", (e) => {
    e.preventDefault();
    install();
  });
});

listen<string>("install-finished", (event) => {
  console.log(`downloading ${event.payload}`);
});
