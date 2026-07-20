(() => {
  const filesEl = document.getElementById("files");
  const previewEl = document.getElementById("preview");
  const statusEl = document.getElementById("status");
  const pathInput = document.getElementById("pathInput");
  const pathLabel = document.getElementById("pathLabel");
  const searchInput = document.getElementById("searchInput");
  let currentPath = "";
  let requestId = 0;
  const pending = new Map();

  const setStatus = (text) => { statusEl.textContent = text; };

  function bridgeCall(name, args = {}) {
    if (window.openai?.callTool) {
      return window.openai.callTool(name, args);
    }
    return new Promise((resolve, reject) => {
      const id = `filesystem-ui-${++requestId}`;
      pending.set(id, { resolve, reject });
      window.parent.postMessage({
        jsonrpc: "2.0",
        id,
        method: "tools/call",
        params: { name, arguments: args }
      }, "*");
    });
  }

  function resultPayload(result) {
    return result?.structuredContent ?? result?.result?.structuredContent ??
      result?.params?.structuredContent ?? result?.toolOutput ?? result ?? {};
  }

  function joinPath(base, name) {
    const slash = base.includes("\\") ? "\\" : "/";
    return `${base.replace(/[\\/]+$/, "")}${slash}${name}`;
  }

  function escapeHtml(value) {
    return String(value).replace(/[&<>"']/g, (char) => ({
      "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;", "'": "&#39;"
    })[char]);
  }

  function normalizeEntries(payload) {
    if (Array.isArray(payload.entries)) {
      return payload.entries.map((entry) => {
        if (typeof entry === "string") {
          const directory = entry.startsWith("[DIR]");
          return {
            name: entry.replace(/^\[(?:DIR|FILE)\]\s*/, ""),
            type: directory ? "directory" : "file"
          };
        }
        return { name: entry.name, type: entry.type === "dir" ? "directory" : entry.type, size: entry.size };
      });
    }
    if (Array.isArray(payload.results)) {
      return payload.results.map((path) => ({ name: String(path), path: String(path), type: "file" }));
    }
    if (payload && payload.type === "directory" && Array.isArray(payload.children)) {
      return payload.children;
    }
    if (Array.isArray(payload.directories)) {
      return payload.directories.map((path) => ({ name: String(path), path: String(path), type: "directory" }));
    }
    return [];
  }

  function renderListing(payload) {
    if (payload.path) {
      currentPath = String(payload.path);
      pathInput.value = currentPath;
      pathLabel.textContent = currentPath;
    }
    const entries = normalizeEntries(payload);
    filesEl.textContent = "";
    for (const entry of entries) {
      const li = document.createElement("li");
      const button = document.createElement("button");
      const target = entry.path || (currentPath ? joinPath(currentPath, entry.name) : entry.name);
      button.innerHTML = `<span>${escapeHtml(entry.name)}</span><span class="kind">${entry.type === "directory" ? "DIR" : entry.size ? `${entry.size} B` : "FILE"}</span>`;
      button.onclick = () => entry.type === "directory" ? openDirectory(target) : openFile(target);
      li.appendChild(button);
      filesEl.appendChild(li);
    }
    setStatus(`${entries.length} item${entries.length === 1 ? "" : "s"}`);
  }

  function renderPreview(payload, fallbackPath = "") {
    const mime = payload.mimeType || payload.detectedType || "";
    const text = payload.content ?? payload.text;
    if (typeof text === "string") {
      if (mime === "text/csv" || fallbackPath.toLowerCase().endsWith(".csv")) {
        const rows = text.split(/\r?\n/).filter(Boolean).map((row) => row.split(","));
        previewEl.innerHTML = `<table>${rows.map((row, index) =>
          `<tr>${row.map((cell) => index === 0 ? `<th>${escapeHtml(cell)}</th>` : `<td>${escapeHtml(cell)}</td>`).join("")}</tr>`
        ).join("")}</table>`;
      } else {
        previewEl.innerHTML = `<pre>${escapeHtml(text)}</pre>`;
      }
      return;
    }
    const data = payload.data ?? payload.blob;
    if (typeof data === "string" && mime.startsWith("image/")) {
      previewEl.innerHTML = `<img alt="File preview" src="data:${escapeHtml(mime)};base64,${data}">`;
      return;
    }
    previewEl.innerHTML = `<pre>${escapeHtml(JSON.stringify(payload, null, 2))}</pre>`;
  }

  async function openDirectory(path) {
    try {
      setStatus("Loading directory…");
      const result = await bridgeCall("list_directory_with_sizes", { path });
      const payload = resultPayload(result);
      if (result?.isError) throw new Error(result?.content?.[0]?.text || "Directory read failed");
      renderListing(payload);
    } catch (error) {
      setStatus(`Error: ${error.message}`);
    }
  }

  async function openFile(path) {
    try {
      setStatus("Loading preview…");
      let result = await bridgeCall("read_text_file", { path });
      if (result?.isError) {
        result = await bridgeCall("read_media_file", { path });
      }
      if (result?.isError) throw new Error(result?.content?.[0]?.text || "File read failed");
      renderPreview(resultPayload(result), path);
      setStatus(path);
    } catch (error) {
      previewEl.innerHTML = `<div class="error">${escapeHtml(error.message)}</div>`;
      setStatus("Preview failed");
    }
  }

  async function search() {
    const pattern = searchInput.value.trim() || "**/*";
    const path = pathInput.value.trim() || currentPath;
    if (!path) return;
    try {
      setStatus("Searching…");
      const result = await bridgeCall("search_files", { path, pattern });
      if (result?.isError) throw new Error(result?.content?.[0]?.text || "Search failed");
      renderListing(resultPayload(result));
    } catch (error) {
      setStatus(`Error: ${error.message}`);
    }
  }

  function consume(result) {
    const payload = resultPayload(result);
    if (payload.content !== undefined || payload.data !== undefined || payload.blob !== undefined) {
      renderPreview(payload, payload.path || "");
    } else {
      renderListing(payload);
    }
  }

  window.addEventListener("message", (event) => {
    if (event.source !== window.parent || !event.data || event.data.jsonrpc !== "2.0") return;
    if (event.data.id && pending.has(event.data.id)) {
      const task = pending.get(event.data.id);
      pending.delete(event.data.id);
      event.data.error ? task.reject(new Error(event.data.error.message)) : task.resolve(event.data.result);
      return;
    }
    if (event.data.method === "ui/notifications/tool-result") consume(event.data.params);
  }, { passive: true });

  window.addEventListener("openai:set_globals", (event) => {
    consume(event.detail?.globals?.toolOutput ?? window.openai?.toolOutput ?? {});
  }, { passive: true });

  document.getElementById("openButton").onclick = () => openDirectory(pathInput.value.trim() || currentPath);
  document.getElementById("refreshButton").onclick = () => currentPath ? openDirectory(currentPath) : bootstrap();
  document.getElementById("searchButton").onclick = search;
  searchInput.addEventListener("keydown", (event) => { if (event.key === "Enter") search(); });

  async function bootstrap() {
    const initial = window.openai?.toolOutput;
    if (initial) {
      consume(initial);
      return;
    }
    try {
      const roots = await bridgeCall("list_allowed_directories", {});
      renderListing(resultPayload(roots));
    } catch {
      setStatus("Call a filesystem browser tool to populate this view.");
    }
  }

  bootstrap();
})();
