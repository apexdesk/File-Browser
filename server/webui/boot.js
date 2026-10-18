import init, { start } from "./filebrowser_web.js";
// El servidor dice si deja modificar archivos (--write)
let writable = false;
try {
  const r = await fetch("../api/info");
  if (r.ok) writable = !!(await r.json()).write;
} catch (e) {}
await init();
await start(document.getElementById("fb_canvas"), writable);
