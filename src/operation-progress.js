import { invoke } from "@tauri-apps/api/core";

export function watchOperation(operationId, { update, error }) {
  let stopped = false;
  let timer;
  async function poll() {
    try {
      const status = await invoke("operation_status", { operationId });
      if (!stopped && status) update(status);
    } catch (failure) {
      if (!stopped) error(failure);
      return;
    }
    if (!stopped) timer = setTimeout(poll, 200);
  }
  timer = setTimeout(poll, 200);
  return () => { stopped = true; clearTimeout(timer); };
}
