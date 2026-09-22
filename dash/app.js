const state = {
  config: null,
  key: "",
  role: "disconnected",
  admin: false,
  managementRead: false,
  managementWrite: false,
  selectedModel: null,
  lastRequest: null,
  promptImages: [],
  keyRoleFilter: "all",
  workerModelFilter: "",
  usagePreset: "7d",
  usageFrom: "",
  usageTo: "",
  usageSort: {
    keys: { field: "time", direction: "desc" },
    models: { field: "time", direction: "desc" },
    workers: { field: "time", direction: "desc" },
  },
  refreshTimer: null,
  refreshIntervalMs: 0,
  disconnected: false,
  sidebarCollapsed: false,
  isRunning: false,
  data: {
    keys: [],
    workers: {},
    connections: {},
    pings: {},
    tags: {},
    versions: {},
    queue: null,
    ollamaTags: null,
    openaiModels: null,
    usage: null,
  },
};

const $ = (selector) => document.querySelector(selector);
const $$ = (selector) => Array.from(document.querySelectorAll(selector));

const apiSurface = [
  {
    title: "Management",
    endpoints: [
      ["GET", "/queue", "admin"],
      ["GET", "/worker/status", "admin"],
      ["GET", "/worker/connections", "admin"],
      ["GET", "/worker/pings", "admin"],
      ["GET", "/worker/tags", "admin"],
      ["GET", "/worker/versions", "admin"],
      ["GET", "/usage?from=YYYY-MM-DD&to=YYYY-MM-DD", "admin"],
      ["GET", "/key", "admin"],
      ["POST", "/key", "admin"],
      ["PATCH", "/key", "admin"],
      ["DELETE", "/key", "admin"],
      ["POST", "/worker/command", "admin"],
    ],
  },
  {
    title: "Ollama Proxy",
    endpoints: [
      ["POST", "/api/generate", "client"],
      ["POST", "/api/chat", "client"],
      ["POST", "/api/embed", "client"],
      ["POST", "/api/embeddings", "client"],
      ["GET", "/api/tags", "client"],
      ["GET", "/api/ps", "client"],
      ["POST", "/api/show", "client"],
      ["GET", "/api/version", "client"],
      ["POST", "/api/create", "admin"],
      ["POST", "/api/copy", "admin"],
      ["POST", "/api/pull", "admin"],
      ["POST", "/api/push", "admin"],
      ["DELETE", "/api/delete", "admin"],
    ],
  },
  {
    title: "OpenAI/vLLM Proxy",
    endpoints: [
      ["POST", "/v1/chat/completions", "client"],
      ["POST", "/v1/chat/completions/batch", "client"],
      ["POST", "/v1/completions", "client"],
      ["POST", "/v1/responses", "client"],
      ["GET", "/v1/responses/:response_id", "client best-effort vllm"],
      ["POST", "/v1/responses/:response_id/cancel", "client best-effort vllm"],
      ["POST", "/v1/embeddings", "client"],
      ["GET", "/v1/models", "client"],
      ["GET", "/v1/models/:model", "client"],
      ["POST", "/v2/embed", "client"],
      ["POST", "/score", "client"],
      ["POST", "/v1/score", "client"],
      ["POST", "/rerank", "client"],
      ["POST", "/v1/rerank", "client"],
      ["POST", "/v2/rerank", "client"],
      ["POST", "/tokenize", "client"],
      ["POST", "/detokenize", "client"],
      ["GET", "/health", "client"],
      ["GET", "/version", "client"],
      ["GET", "/tokenizer_info", "client"],
      ["GET", "/is_sleeping", "client"],
      ["GET", "/load", "admin"],
      ["GET", "/metrics", "admin targeted"],
      ["POST", "/v1/load_lora_adapter", "admin targeted"],
      ["POST", "/v1/unload_lora_adapter", "admin targeted"],
      ["POST", "/v1/lora_adapters", "admin targeted"],
      ["POST", "/start_profile", "admin targeted"],
      ["POST", "/stop_profile", "admin targeted"],
      ["POST", "/sleep", "admin targeted"],
      ["POST", "/wake_up", "admin targeted"],
    ],
  },
];

document.addEventListener("DOMContentLoaded", init);

async function init() {
  bindEvents();
  renderApiSurface();
  try {
    state.config = await requestJson("/config.json");
  } catch {
    // If config fetch fails, still allow manual login
    state.config = { proxyEndpoint: "", managementEndpoint: "", key: "" };
  }
  $("#proxy-endpoint").textContent = state.config.proxyEndpoint;
  $("#management-endpoint").textContent = state.config.managementEndpoint;
  $("#login-proxy-endpoint").textContent = state.config.proxyEndpoint;
  $("#login-management-endpoint").textContent = state.config.managementEndpoint;
  if (state.config.key) {
    $("#key-input").value = state.config.key;
    state.key = state.config.key;
    await connect();
  }
}

function bindEvents() {
  $("#login-form").addEventListener("submit", async (event) => {
    event.preventDefault();
    const btn = $("#login-button");
    btn.disabled = true;
    btn.textContent = "Connecting...";
    state.key = $("#key-input").value.trim();
    await connect();
    btn.disabled = false;
    btn.textContent = "Connect";
  });

  $("#logout-button").addEventListener("click", () => {
    disconnectSession();
  });

  $("#toggle-key").addEventListener("click", () => {
    const input = $("#key-input");
    input.type = input.type === "password" ? "text" : "password";
  });

  $("#refresh-button").addEventListener("click", () => refreshCurrentView());

  // Sidebar toggle
  $("#sidebar-toggle").addEventListener("click", () => {
    state.sidebarCollapsed = !state.sidebarCollapsed;
    $("#sidebar").classList.toggle("collapsed", state.sidebarCollapsed);
  });

  // Reconnect button
  $("#reconnect-button").addEventListener("click", async () => {
    $("#disconnect-banner").classList.add("hidden");
    state.disconnected = false;
    await connect();
  });

  $("#refresh-interval").addEventListener("change", () => {
    state.refreshIntervalMs = Number($("#refresh-interval").value);
    configureAutoRefresh();
  });

  $$(".nav-button").forEach((button) => {
    button.addEventListener("click", () => showView(button.dataset.view));
  });

  // Copy response
  $("#copy-response-button").addEventListener("click", () => {
    const text = $("#prompt-output").textContent;
    if (!text) return;
    navigator.clipboard.writeText(text).then(() => {
      toast("Response copied to clipboard");
    }).catch(() => {
      fallbackCopy(text);
      toast("Response copied to clipboard");
    });
  });

  // Clear response
  $("#clear-response-button").addEventListener("click", () => {
    setPromptOutput("", true);
    $("#copy-response-button").style.display = "none";
    $("#clear-response-button").style.display = "none";
  });

  document.addEventListener("click", async (event) => {
    const actionTarget = event.target?.closest?.("[data-action]");
    const action = actionTarget?.dataset?.action;
    if (!action) return;
    if (action === "load-ollama-tags") return loadOllamaTags();
    if (action === "load-openai-models") return loadOpenAiModels();
    if (action === "load-keys") return loadKeys();
    if (action === "load-workers") return loadWorkers();
    if (action === "load-queue") return loadQueue();
    if (action === "sort-usage") {
      return sortUsageTable(
        actionTarget.dataset.table,
        actionTarget.dataset.field,
      );
    }
    if (action === "remove-prompt-image")
      return removePromptImage(actionTarget.dataset.id);
    if (action === "delete-key")
      return deleteKey(Number(actionTarget.dataset.id));
    if (action === "copy-token") return copyToken(actionTarget.dataset.token);
    if (action === "select-model") {
      return selectModel(
        actionTarget.dataset.source,
        actionTarget.dataset.model,
        actionTarget.dataset.defaultMode,
      );
    }
    if (action === "api-endpoint") {
      return openApiPlay(actionTarget.dataset.method, actionTarget.dataset.path, actionTarget.dataset.section);
    }
    if (action === "api-play") {
      return openApiPlay(actionTarget.dataset.method, actionTarget.dataset.path, actionTarget.dataset.section);
    }
    if (action === "api-play-close") {
      return closeApiPlay();
    }
  });

  $("#create-key-form").addEventListener("submit", createKey);

  // Auto-save on change for inline key edits
  $("#keys-table").addEventListener("change", (event) => {
    const el = event.target;
    const id = el.dataset.autoSave;
    if (id) saveKey(Number(id));
  });
  $("#worker-command-form").addEventListener("submit", sendWorkerCommand);
  $("#worker-model-filter").addEventListener("input", () => {
    state.workerModelFilter = $("#worker-model-filter").value.trim();
    renderWorkers();
  });
  $("#usage-range-form").addEventListener("submit", async (event) => {
    event.preventDefault();
    state.usagePreset = $("#usage-preset").value;
    state.usageFrom = $("#usage-from").value;
    state.usageTo = $("#usage-to").value;
    await loadUsage();
  });
  $("#usage-preset").addEventListener("change", async () => {
    state.usagePreset = $("#usage-preset").value;
    applyUsagePreset();
    await loadUsage();
  });
  $("#prompt-form").addEventListener("submit", runPrompt);
  $("#prompt-image-button").addEventListener("click", () => {
    const input = $("#prompt-images");
    if (!input.disabled) input.click();
  });
  $("#prompt-images").addEventListener("change", handlePromptImages);
  $("#model-console-mode").addEventListener("change", () => {
    if (state.selectedModel) {
      state.selectedModel.mode = $("#model-console-mode").value;
      renderModelConsole();
    }
  });
  $("#model-console-worker").addEventListener("change", () => {
    if (state.selectedModel) {
      state.selectedModel.worker = $("#model-console-worker").value;
    }
  });
  $$("#key-role-tabs .tab").forEach((button) => {
    button.addEventListener("click", () => {
      state.keyRoleFilter = button.dataset.roleFilter;
      renderKeys();
    });
  });

  // Keyboard shortcuts
  document.addEventListener("keydown", (event) => {
    // Don't trigger shortcuts when typing in inputs
    const tag = event.target.tagName;
    if (tag === "INPUT" || tag === "TEXTAREA" || tag === "SELECT") {
      // Ctrl+Enter still works in textarea
      if (event.key === "Enter" && (event.ctrlKey || event.metaKey)) {
        const form = event.target.closest("form");
        if (form) form.requestSubmit();
      }
      return;
    }

    // Navigation: 1-6
    const viewIndex = parseInt(event.key);
    if (viewIndex >= 1 && viewIndex <= 6) {
      const views = ["overview", "workers", "models", "keys", "stats", "api"];
      const nav = document.querySelector(`.nav-button[data-view="${views[viewIndex - 1]}"]`);
      if (nav && !nav.classList.contains("hidden")) {
        showView(views[viewIndex - 1]);
      }
      return;
    }

    // R for refresh
    if (event.key === "r" || event.key === "R") {
      refreshCurrentView();
    }
  });
}

function disconnectSession() {
  if (state.refreshTimer) {
    clearInterval(state.refreshTimer);
    state.refreshTimer = null;
  }
  $("#dashboard-shell").classList.add("hidden");
  $("#login-screen").classList.remove("hidden");
  state.role = "disconnected";
  state.admin = false;
  state.managementRead = false;
  state.managementWrite = false;
  state.disconnected = false;
  state.isRunning = false;
  $("#disconnect-banner").classList.add("hidden");
  renderRole();
  setLoginStatus("Enter a key to connect.");
}

async function connect() {
  if (!state.key) {
    setLoginStatus("Enter a Hive key.", true);
    return;
  }
  showSkeleton(true);
  setLoginStatus("Checking key...");
  resetData();

  const adminProbe = await api("management", "/key", {
    expected: [200, 403, 401],
  });
  if (adminProbe.status === 200) {
    const canSeeTokens = Array.isArray(adminProbe.body)
      ? adminProbe.body.some((key) => typeof key.token === "string" && key.token.length > 0)
      : false;
    state.admin = canSeeTokens;
    state.managementRead = true;
    state.managementWrite = canSeeTokens;
    state.role = canSeeTokens ? "admin" : "analytics";
    // Admin: 5s auto-refresh, non-admin: 0
    state.refreshIntervalMs = canSeeTokens ? 5000 : 0;
    $("#refresh-interval").value = String(state.refreshIntervalMs);
    state.data.keys = adminProbe.body;
    setStatus(canSeeTokens ? "Connected as admin." : "Connected as analytics.");
    await Promise.all([loadWorkers(), loadQueue(), loadOllamaTags(), loadOpenAiModels(), loadMyKey()]);
  } else {
    const modelProbe = await api("proxy", "/api/tags", {
      expected: [200, 401, 403, 404, 500],
    });
    if (modelProbe.status === 401 || modelProbe.status === 403) {
      state.role = "disconnected";
      state.admin = false;
      state.managementRead = false;
      state.managementWrite = false;
      setLoginStatus("Key was rejected by HiveCore.", true);
      renderRole();
      showSkeleton(false);
      return;
    }
    state.admin = false;
    state.managementRead = false;
    state.managementWrite = false;
    state.role = "client";
    state.refreshIntervalMs = 0;
    $("#refresh-interval").value = "0";
    setStatus("Connected as client.");
    if (modelProbe.status === 200) state.data.ollamaTags = modelProbe.body;
    await Promise.all([loadOpenAiModels({ quiet: true }), loadMyKey()]);
  }

  renderRole();
  renderAll();
  $("#login-screen").classList.add("hidden");
  $("#dashboard-shell").classList.remove("hidden");
  showSkeleton(false);
  configureAutoRefresh();
}

function showSkeleton(visible) {
  $("#loading-skeleton").classList.toggle("hidden", !visible);
  // Hide views while skeleton is shown
  $$(".view").forEach((view) => {
    if (visible) view.classList.remove("active");
  });
}

function resetData() {
  state.data = {
    keys: [],
    workers: {},
    connections: {},
    pings: {},
    tags: {},
    versions: {},
    queue: null,
    ollamaTags: null,
    openaiModels: null,
    usage: null,
    myKey: null,
    myLimits: null,
    myUsage: null,
  };
  state.selectedModel = null;
  state.lastRequest = null;
  state.promptImages = [];
}

function renderRole() {
  const pill = $("#role-pill");
  pill.className = `pill ${
    state.role === "admin"
      ? "admin"
      : state.role === "analytics"
        ? "analytics"
        : state.role === "client"
          ? "client"
          : ""
  }`;
  pill.textContent =
    state.role === "admin"
      ? "Admin key"
      : state.role === "analytics"
        ? "Analytics key"
        : state.role === "client"
          ? "Client key"
          : "Disconnected";
  $("#metric-role").textContent = state.role;
  $(".refresh-control").classList.toggle("hidden", !state.managementRead);
  $$(".admin-only").forEach((element) => {
    element.classList.toggle("hidden", !state.admin);
  });
  $$(".management-read").forEach((element) => {
    element.classList.toggle("hidden", !state.managementRead);
  });
  $$(".admin-write").forEach((element) => {
    element.classList.toggle("hidden", !state.managementWrite);
  });
  renderApiSurface();
  if (!state.managementRead && ["keys", "workers", "stats"].includes(currentView())) {
    showView("overview");
  }
}

function configureAutoRefresh() {
  if (state.refreshTimer) {
    clearInterval(state.refreshTimer);
    state.refreshTimer = null;
  }
  if (!state.managementRead || !state.refreshIntervalMs) return;
  state.refreshTimer = setInterval(() => {
    if (state.disconnected) return;
    refreshCurrentView({ soft: true }).catch((error) => {
      handleFetchError(error);
    });
  }, state.refreshIntervalMs);
}

function handleFetchError(error) {
  if (state.disconnected) return;
  state.disconnected = true;
  if (state.refreshTimer) {
    clearInterval(state.refreshTimer);
    state.refreshTimer = null;
  }
  $("#disconnect-banner").classList.remove("hidden");
  toast("Connection lost: " + (error.message || error), true);
}

function showView(name) {
  $$(".nav-button").forEach((button) => {
    button.classList.toggle("active", button.dataset.view === name);
  });
  $$(".view").forEach((view) => view.classList.remove("active"));
  $(`#${name}-view`).classList.add("active");
  $("#page-title").textContent = titleCase(name);
  refreshCurrentView({ soft: true });
}

function currentView() {
  return $(".nav-button.active")?.dataset.view || "overview";
}

async function refreshCurrentView(options = {}) {
  if (!state.key || state.disconnected) return;
  const view = currentView();
  if (!options.soft) setStatus("Refreshing...");
  if (view === "overview") {
    await Promise.all([
      state.managementRead ? loadWorkers({ quiet: true }) : Promise.resolve(),
      state.managementRead ? loadQueue({ quiet: true }) : Promise.resolve(),
      loadOllamaTags({ quiet: true }),
      loadOpenAiModels({ quiet: true }),
      loadMyKey(),
    ]);
  }
  if (view === "models")
    await Promise.all([
      loadOllamaTags({ quiet: true }),
      loadOpenAiModels({ quiet: true }),
    ]);
  if (view === "keys" && state.managementRead) await loadKeys({ quiet: true });
  if (view === "workers" && state.managementRead)
    await Promise.all([
      loadWorkers({ quiet: true }),
      loadQueue({ quiet: true }),
    ]);
  if (view === "stats" && state.managementRead) await loadUsage({ quiet: true });
  renderAll();
  if (!options.soft) setStatus("Refreshed.");
}

async function loadKeys() {
  const response = await api("management", "/key");
  state.data.keys = response.body;
  renderKeys();
}

async function loadMyKey() {
  const today = localDateString(new Date());
  const [meRes, limitsRes, usageRes] = await Promise.all([
    api("management", "/key/me", { expected: [200, 403, 401] }),
    api("management", "/key/me/limits", { expected: [200, 403, 401] }),
    api("management", `/key/me/usage?from=${today}&to=${today}`, { expected: [200, 403, 401] }),
  ]);
  if (meRes.status === 200) state.data.myKey = meRes.body;
  if (limitsRes.status === 200) state.data.myLimits = limitsRes.body;
  if (usageRes.status === 200) state.data.myUsage = usageRes.body;
  renderMyKey();
}

async function loadWorkers() {
  const [workers, connections, pings, tags, versions] = await Promise.all([
    api("management", "/worker/status"),
    api("management", "/worker/connections"),
    api("management", "/worker/pings"),
    api("management", "/worker/tags"),
    api("management", "/worker/versions"),
  ]);
  state.data.workers = workers.body;
  state.data.connections = connections.body;
  state.data.pings = pings.body;
  state.data.tags = tags.body;
  state.data.versions = versions.body;
  renderWorkers();
  renderModelWorkerSelect();
}

async function loadQueue() {
  const response = await api("management", "/queue");
  state.data.queue = response.body;
  renderQueue();
}

async function loadUsage() {
  if (!state.managementRead) return;
  ensureUsageRange();
  const query = `?from=${encodeURIComponent(state.usageFrom)}&to=${encodeURIComponent(state.usageTo)}`;
  const response = await api("management", `/usage${query}`, {
    expected: [200, 400, 500],
  });
  if (response.status !== 200) {
    state.data.usage = { error: response.body || response.status };
  } else {
    state.data.usage = response.body;
  }
  renderStats();
}

async function loadOllamaTags() {
  const response = await api("proxy", "/api/tags", {
    expected: [200, 401, 403, 404, 500],
  });
  state.data.ollamaTags =
    response.status === 200
      ? response.body
      : { error: response.body || response.status };
  renderModels();
}

async function loadOpenAiModels() {
  const response = await api("proxy", "/v1/models", {
    expected: [200, 401, 403, 404, 500],
  });
  state.data.openaiModels =
    response.status === 200
      ? response.body
      : { error: response.body || response.status };
  renderModels();
}

function renderAll() {
  renderRole();
  renderMyKey();
  renderOverview();
  renderModels();
  if (state.managementRead) {
    renderKeys();
    renderWorkers();
    renderQueue();
    renderStats();
  }
}

function renderOverview() {
  const workerCount = Object.keys(state.data.workers || {}).length;
  const ollamaModels = extractOllamaModels(state.data.ollamaTags);
  const openaiModels = extractOpenAiModels(state.data.openaiModels);
  const modelIds = new Set(
    [
      ...ollamaModels.map((model) => model.name || model.model || model),
      ...openaiModels.map((model) => model.id || model),
    ].filter(Boolean),
  );
  const queued = state.data.queue
    ? Object.values(state.data.queue.model_queue || {}).reduce(
        (sum, value) => sum + Number(value || 0),
        0,
      ) +
      Object.values(state.data.queue.node_queue || {}).reduce(
        (sum, value) => sum + Number(value || 0),
        0,
      )
    : 0;

  $("#metric-workers").textContent = state.managementRead ? workerCount : "-";
  $("#metric-models").textContent = modelIds.size || "-";
  $("#metric-queued").textContent = state.managementRead ? queued : "-";
  $("#overview-workers")
    .closest(".panel")
    .classList.toggle("hidden", !state.managementRead);
  $("#overview-queues")
    .closest(".panel")
    .classList.toggle("hidden", !state.managementRead);
  $("#overview-backends")
    .closest(".panel")
    .classList.toggle("hidden", !state.managementRead);
  renderOverviewWorkers();
  renderOverviewQueues(queued);
  renderOverviewBackends();
  renderOverviewModels(ollamaModels, openaiModels);
}

function renderOverviewWorkers() {
  const root = $("#overview-workers");
  if (!state.managementRead) {
    root.innerHTML = "";
    return;
  }
  const workers = workerNames();
  if (!workers.length) {
    root.innerHTML = mutedBlock("No connected workers.");
    return;
  }
  const visible = workers.map((name) => overviewWorkerCard(name));
  root.className = "overview-worker-grid";
  root.innerHTML = visible.join("");
}

function renderOverviewQueues(queued) {
  const root = $("#overview-queues");
  if (!state.managementRead) {
    root.innerHTML = "";
    return;
  }
  const queue = state.data.queue || { model_queue: {}, node_queue: {} };
  if (!queued) {
    root.innerHTML = overviewRow("All queues", "Idle");
    return;
  }
  const rows = [
    ...Object.entries(queue.model_queue || {})
      .filter(([, count]) => Number(count) > 0)
      .map(([name, count]) => overviewRow(name, `${count} model queued`)),
    ...Object.entries(queue.node_queue || {})
      .filter(([, count]) => Number(count) > 0)
      .map(([name, count]) => overviewRow(name, `${count} node queued`)),
  ];
  root.innerHTML = rows.join("");
}

function renderOverviewBackends() {
  const root = $("#overview-backends");
  if (!state.managementRead) {
    root.innerHTML = "";
    return;
  }
  const counts = {};
  for (const worker of Object.values(state.data.workers || {})) {
    const backend = worker.backend || "unknown";
    counts[backend] = (counts[backend] || 0) + 1;
  }
  const entries = Object.entries(counts).sort(([a], [b]) => a.localeCompare(b));
  root.innerHTML = entries.length
    ? entries
        .map(([backend, count]) =>
          overviewRow(backend, `${count} worker${count === 1 ? "" : "s"}`),
        )
        .join("")
    : mutedBlock("No backend data.");
}

function renderOverviewModels(ollamaModels, openaiModels) {
  const root = $("#overview-models");
  const embeddingCount = ollamaModels.filter(
    (model) => defaultModeForModel(model) === "embedding",
  ).length;
  root.innerHTML = [
    overviewRow(
      "Ollama visible",
      `${ollamaModels.length} model${ollamaModels.length === 1 ? "" : "s"}`,
    ),
    overviewRow(
      "OpenAI visible",
      `${openaiModels.length} model${openaiModels.length === 1 ? "" : "s"}`,
    ),
    overviewRow(
      "Embedding-likely",
      `${embeddingCount} Ollama model${embeddingCount === 1 ? "" : "s"}`,
    ),
  ].join("");
}

function overviewRow(label, value) {
  return `
    <div class="overview-row">
      <span>${escapeHtml(label)}</span>
      <strong>${escapeHtml(value)}</strong>
    </div>
  `;
}

function mutedBlock(text) {
  return `<div class="muted-block">${escapeHtml(text)}</div>`;
}

function renderModels() {
  renderModelList(
    "#ollama-models",
    "ollama",
    extractOllamaModels(state.data.ollamaTags),
    state.data.ollamaTags,
  );
  renderModelList(
    "#openai-models",
    "openai",
    extractOpenAiModels(state.data.openaiModels),
    state.data.openaiModels,
  );
  renderModelConsole();
  renderRequestInfo();
  renderOverview();
}

function renderModelList(selector, source, models, raw) {
  const root = $(selector);
  if (!raw) {
    root.className = "list-empty";
    root.textContent = "No data loaded.";
    return;
  }
  if (raw.error) {
    root.className = "list-empty";
    root.textContent = `Unavailable: ${stringifyError(raw.error)}`;
    return;
  }
  if (!models.length) {
    root.className = "list-empty";
    root.textContent = "No models returned.";
    return;
  }
  root.className = "table-wrap";
  root.innerHTML = table(
    ["Model", "Type", "Details"],
    models.map((model) => {
      const name = model.name || model.model || model.id || String(model);
      const defaultMode = defaultModeForModel(model);
      const details = model.size
        ? formatBytes(model.size)
        : model.owned_by || model.object || model.modified_at || "";
      return [
        `<button class="link-button" data-action="select-model" data-source="${source}" data-model="${escapeAttr(name)}" data-default-mode="${defaultMode}">${escapeHtml(name)}</button>`,
        escapeHtml(defaultMode),
        escapeHtml(details),
      ];
    }),
  );
}

function renderKeys() {
  if (!state.managementRead) return;
  const keys = (state.data.keys || []).filter((key) => {
    return state.keyRoleFilter === "all" || key.role === state.keyRoleFilter;
  });
  $$("#key-role-tabs .tab").forEach((button) => {
    button.classList.toggle(
      "active",
      button.dataset.roleFilter === state.keyRoleFilter,
    );
  });
  if (!keys.length) {
    $("#keys-table").innerHTML = empty();
    return;
  }
  const headers = state.managementWrite
    ? ["ID", "Name", "Token", "Role", "Rate Limit", "Capture", "Whitelist", "Blacklist", ""]
    : ["ID", "Name", "Role", "Rate Limit", "Capture", "Whitelist", "Blacklist"];
  const rows = keys.map((key) => {
    const tierOptions = ["low", "medium", "high", "unlimited"]
      .map((t) => `<option value="${t}"${t === key.rate_limit_tier ? " selected" : ""}>${t}</option>`)
      .join("");
    const common = state.managementWrite
      ? [
          key.id,
          `<input data-key-name="${key.id}" value="${escapeAttr(key.name)}" data-auto-save="${key.id}" />`,
          key.token
            ? `<button class="token-copy" data-action="copy-token" data-token="${escapeAttr(key.token)}" title="Copy token">${escapeHtml(maskToken(key.token))}</button>`
            : `<span class="muted">redacted</span>`,
          escapeHtml(key.role),
          `<select class="tier-select" data-key-tier="${key.id}" data-auto-save="${key.id}">${tierOptions}</select>`,
          `<label class="check"><input data-key-capture="${key.id}" type="checkbox" ${key.capture ? "checked" : ""} data-auto-save="${key.id}" /> capture</label>`,
          tags(key.whitelist_models),
          tags(key.blacklist_models),
          `<button class="small" data-action="delete-key" data-id="${key.id}">Delete</button>`,
        ]
      : [
          key.id,
          escapeHtml(key.name),
          escapeHtml(key.role),
          escapeHtml(key.rate_limit_tier || "low"),
          key.capture ? "yes" : "no",
          tags(key.whitelist_models),
          tags(key.blacklist_models),
        ];
    return common;
  });
  $("#keys-table").innerHTML = table(headers, rows);
}

function renderMyKey() {
  const myKey = state.data.myKey;
  const myLimits = state.data.myLimits;
  const myUsage = state.data.myUsage;
  const root = $("#overview-my-limits");
  if (!myKey) {
    root.innerHTML = `<div class="list-empty">Load key info to see limits.</div>`;
    return;
  }
  const tier = myKey.rate_limit_tier || "low";
  const tierCfg = myKey.tier || {};
  const maxConcurrent = tierCfg.max_concurrent;
  const snapshot = (myLimits && myLimits.snapshot) || (myKey.limits) || null;

  let html = `<div class="my-limits-grid">`;
  html += `<article class="usage-stat stat-gray"><span>Tier</span><strong>${escapeHtml(tier)}</strong></article>`;

  // Concurrent
  if (maxConcurrent != null && maxConcurrent > 0) {
    const current = (snapshot && snapshot.concurrent) || 0;
    const pct = Math.min(100, Math.round((current / maxConcurrent) * 100));
    const cls = current === 0 ? "stat-gray" : pct >= 100 ? "stat-red" : pct > 50 ? "stat-yellow" : "stat-green";
    html += `<article class="usage-stat ${cls}">
      <span>Concurrent</span>
      <strong>${current}/${maxConcurrent}</strong>
      <div class="usage-bar"><span style="width:${pct}%"></span></div>
    </article>`;
  } else {
    html += `<article class="usage-stat stat-gray"><span>Concurrent</span><strong>0/−</strong></article>`;
  }

  const winMap = {};
  if (snapshot && snapshot.windows) {
    for (const win of snapshot.windows) {
      winMap[win.name] = win.count;
    }
  }

  const windowNames = [
    { key: "min", limitField: "requests_per_minute", label: "per min" },
    { key: "hour", limitField: "requests_per_hour", label: "per hour" },
    { key: "day", limitField: "requests_per_day", label: "per day" },
    { key: "week", limitField: "requests_per_week", label: "per week" },
    { key: "month", limitField: "requests_per_month", label: "per month" },
  ];
  for (const w of windowNames) {
    const limit = tierCfg[w.limitField];
    if (limit != null && limit > 0) {
      const count = winMap[w.key] || 0;
      const pct = Math.min(100, Math.round((count / limit) * 100));
      const cls = count === 0 ? "stat-gray" : pct >= 100 ? "stat-red" : pct > 50 ? "stat-yellow" : "stat-green";
      html += `<article class="usage-stat ${cls}">
        <span>${w.label}</span>
        <strong>${count}/${limit}</strong>
        <div class="usage-bar"><span style="width:${pct}%"></span></div>
      </article>`;
    }
  }

  html += `</div>`;

  if (myUsage && myUsage.rows && myUsage.rows.length) {
    const rows = myUsage.rows;
    let totalReqs = 0, totalErrors = 0, totalPrompt = 0, totalCompletion = 0;
    for (const row of rows) {
      totalReqs += Number(row.request_count || 0);
      totalErrors += Number(row.error_count || 0);
      totalPrompt += Number(row.prompt_tokens || 0);
      totalCompletion += Number(row.completion_tokens || 0);
    }
    html += `<div class="my-limits-grid" style="margin-top:10px">`;
    html += `<article class="usage-stat stat-gray"><span>Today</span><strong>${formatCount(totalReqs)}</strong></article>`;
    html += `<article class="usage-stat ${totalErrors > 0 ? "stat-red" : "stat-green"}"><span>Errors</span><strong>${formatCount(totalErrors)}</strong></article>`;
    html += `<article class="usage-stat stat-gray"><span>Input</span><strong>${formatCount(totalPrompt)}</strong></article>`;
    html += `<article class="usage-stat stat-gray"><span>Output</span><strong>${formatCount(totalCompletion)}</strong></article>`;
    html += `</div>`;
  }

  root.innerHTML = html;
}

function renderWorkers() {
  if (!state.managementRead) return;
  const names = workerNames()
    .filter((name) => workerMatchesModelFilter(name, state.workerModelFilter))
    .sort((a, b) => a.localeCompare(b));

  if (!names.length) {
    $("#workers-grid").innerHTML = state.workerModelFilter
      ? `<div class="list-empty">No workers match this model filter.</div>`
      : empty();
    return;
  }

  const totalConnections = names.reduce((sum, name) => {
    return sum + Number(state.data.connections[name]?.connections || 1);
  }, 0);
  const working = names.reduce((sum, name) => {
    const connection = state.data.connections[name] || {};
    return sum + Number(connection.working_connections || 0);
  }, 0);
  $("#workers-grid").innerHTML = `
    <div class="worker-summary">
      <strong>${working}/${totalConnections}</strong>
      <span>connections working</span>
    </div>
    ${names
      .map((name) => {
        const worker = state.data.workers[name] || {};
        const connection = state.data.connections[name] || {};
        const ping = state.data.pings[name] || {};
        const version = state.data.versions[name] || {};
        const models = state.data.tags[name] || [];
        const load = workerLoad(name);
        const pollMs = Number(ping.last_poll_ms);
        return `
        <article class="worker-card ${load.className}">
          <div class="worker-card-head">
            <strong>${escapeHtml(name)}</strong>
            <span>${escapeHtml(workerBackendLabel(connection.backend || worker.backend || version.backend || "-"))}</span>
          </div>
          <div class="worker-bar" title="${load.working}/${load.total} working">
            <span style="width: ${load.percent}%"></span>
          </div>
          <div class="worker-foot">
            <span>${load.working}/${load.total} working</span>
            ${Number.isFinite(pollMs) && pollMs > 10_000 ? `<span>poll ${formatMs(pollMs)}</span>` : ""}
          </div>
          <div class="worker-models">${tags(models)}</div>
        </article>
      `;
      })
      .join("")}
  `;
}

function workerBackendLabel(value) {
  if (value === "ollama_legacy") return "legacy_ollama";
  return value || "-";
}

function workerNames() {
  const names = Array.from(
    new Set([
      ...Object.keys(state.data.workers || {}),
      ...Object.keys(state.data.connections || {}),
      ...Object.keys(state.data.pings || {}),
      ...Object.keys(state.data.tags || {}),
      ...Object.keys(state.data.versions || {}),
    ]),
  );
  names.sort((a, b) => {
    const aLoad = workerLoad(a);
    const bLoad = workerLoad(b);
    return (
      bLoad.working - aLoad.working ||
      bLoad.percent - aLoad.percent ||
      a.localeCompare(b)
    );
  });
  return names;
}

function workerMatchesModelFilter(name, filter) {
  if (!filter) return true;
  const needle = filter.toLowerCase();
  return (state.data.tags[name] || []).some((model) =>
    String(model).toLowerCase().includes(needle),
  );
}

function workerLoad(name) {
  const connection = state.data.connections[name] || {};
  const total = Number(connection.connections || 1);
  const working = Number(connection.working_connections || 0);
  const percent = total ? Math.round((working / total) * 100) : 0;
  return {
    total,
    working,
    percent,
    className: workerLoadClass(percent),
  };
}

function workerLoadClass(percent) {
  if (percent >= 100) return "load-red";
  if (percent > 50) return "load-yellow";
  if (percent > 0) return "load-green";
  return "load-gray";
}

function overviewWorkerCard(name) {
  const load = workerLoad(name);
  return `
    <article class="overview-worker-card ${load.className}">
      <div class="overview-worker-head">
        <strong>${escapeHtml(name)}</strong>
        <span>${load.working}/${load.total}</span>
      </div>
      <div class="worker-bar" title="${load.working}/${load.total} working">
        <span style="width: ${load.percent}%"></span>
      </div>
    </article>
  `;
}

function renderQueue() {
  if (!state.managementRead) return;
  const root = $("#queue-view");
  const queue = state.data.queue || {};
  const modelQueue = queue.model_queue || {};
  const nodeQueue = queue.node_queue || {};
  const modelEntries = Object.entries(modelQueue).filter(([, count]) => Number(count) > 0);
  const nodeEntries = Object.entries(nodeQueue).filter(([, count]) => Number(count) > 0);

  if (!modelEntries.length && !nodeEntries.length) {
    root.innerHTML = `<div class="queue-empty">All queues idle</div>`;
    return;
  }

  let html = "";
  if (modelEntries.length) {
    html += `<div class="queue-card">
      <h4>Model Queue</h4>
      <div class="queue-rows">
        ${modelEntries.map(([name, count]) => `
          <div class="queue-row">
            <strong>${escapeHtml(name)}</strong>
            <span>${count} queued</span>
          </div>
        `).join("")}
      </div>
    </div>`;
  }
  if (nodeEntries.length) {
    html += `<div class="queue-card">
      <h4>Node Queue</h4>
      <div class="queue-rows">
        ${nodeEntries.map(([name, count]) => `
          <div class="queue-row">
            <strong>${escapeHtml(name)}</strong>
            <span>${count} queued</span>
          </div>
        `).join("")}
      </div>
    </div>`;
  }
  root.innerHTML = html;
  renderOverview();
}

function ensureUsageRange() {
  if (!state.usagePreset) state.usagePreset = "today";
  if (!state.usageFrom || !state.usageTo) applyUsagePreset(false);
}

function applyUsagePreset(updateControls = true) {
  const today = localDateString(new Date());
  const date = new Date(`${today}T00:00:00`);
  let from = today;
  let to = today;

  if (state.usagePreset === "yesterday") {
    date.setDate(date.getDate() - 1);
    from = localDateString(date);
    to = from;
  } else if (state.usagePreset === "7d") {
    date.setDate(date.getDate() - 6);
    from = localDateString(date);
  } else if (state.usagePreset === "30d") {
    date.setDate(date.getDate() - 29);
    from = localDateString(date);
  } else if (state.usagePreset === "custom") {
    from = state.usageFrom || today;
    to = state.usageTo || today;
  }

  state.usageFrom = from;
  state.usageTo = to;
  if (updateControls) syncUsageControls();
}

function syncUsageControls() {
  const preset = $("#usage-preset");
  if (!preset) return;
  preset.value = state.usagePreset || "today";
  $("#usage-from").value = state.usageFrom || "";
  $("#usage-to").value = state.usageTo || "";
  const custom = state.usagePreset === "custom";
  $("#usage-from").disabled = !custom;
  $("#usage-to").disabled = !custom;
}

function renderStats() {
  if (!state.managementRead) return;
  ensureUsageRange();
  syncUsageControls();
  const usage = state.data.usage;
  if (!usage) {
    $("#usage-status").textContent = "No usage loaded.";
    $("#usage-summary").innerHTML = "";
    $("#usage-traffic-chart").innerHTML = mutedBlock("Load usage stats.");
    $("#usage-request-chart").innerHTML = mutedBlock("Load usage stats.");
    $("#usage-worker-chart").innerHTML = mutedBlock("Load usage stats.");
    $("#usage-days").innerHTML = mutedBlock("Load usage stats.");
    $("#usage-backends").innerHTML = mutedBlock("Load usage stats.");
    $("#usage-keys").innerHTML = empty();
    $("#usage-models").innerHTML = empty();
    $("#usage-workers").innerHTML = empty();
    return;
  }
  if (usage.error) {
    $("#usage-status").textContent = `Usage unavailable: ${stringifyError(usage.error)}`;
    $("#usage-summary").innerHTML = "";
    $("#usage-traffic-chart").innerHTML = mutedBlock("No usage available.");
    $("#usage-request-chart").innerHTML = mutedBlock("No request data available.");
    $("#usage-worker-chart").innerHTML = mutedBlock("No worker data available.");
    $("#usage-days").innerHTML = mutedBlock("No usage available.");
    $("#usage-backends").innerHTML = mutedBlock("No backend usage available.");
    $("#usage-keys").innerHTML = empty();
    $("#usage-models").innerHTML = empty();
    $("#usage-workers").innerHTML = empty();
    return;
  }

  const summary = usage.summary || {};
  const requests = statValue(summary, "requests");
  const errors = statValue(summary, "errors");
  const totalMs = statDuration(summary);
  const outputTokens = statValue(summary, "completion_tokens");
  const totalTokens = statTotalTokens(summary);
  const errorRate = requests ? `${formatRate((errors / requests) * 100)}%` : "-";
  const outputTps = totalMs ? formatRate(outputTokens / (totalMs / 1000)) : "-";
  const totalTps = totalMs ? formatRate(totalTokens / (totalMs / 1000)) : "-";

  $("#usage-status").textContent =
    `Showing ${usage.from || state.usageFrom} through ${usage.to || state.usageTo}.`;
  $("#usage-summary").innerHTML = [
    statCard("Requests", formatCount(requests), `${formatCount(statValue(summary, "success"))} ok`),
    statCard("Errors", formatCount(errors), `${errorRate} error rate`, errors ? "bad" : "good"),
    statCard("Input", formatCount(statValue(summary, "prompt_tokens")), "prompt tokens"),
    statCard("Output", formatCount(outputTokens), "completion tokens"),
    statCard("Total Tokens", formatCount(totalTokens), `${totalTps} tok/s`),
    statCard("Worker Time", formatDuration(totalMs), `${outputTps} output tok/s`),
    statCard("Queue Time", formatDuration(statValue(summary, "queue_ms")), "summed wait"),
  ].join("");

  renderUsageBars("#usage-days", usage.days || [], "day", "requests", "day", "desc");
  renderUsageBars("#usage-backends", usage.backends || [], "backend", "requests");
  renderUsageCharts(usage);
  renderUsageKeys(usage.keys || []);
  renderUsageModels(usage.models || []);
  renderUsageWorkers(usage.workers || []);
}

function statCard(label, value, detail, tone = "") {
  return `
    <article class="usage-stat ${tone}">
      <span>${escapeHtml(label)}</span>
      <strong>${escapeHtml(value)}</strong>
      <small>${escapeHtml(detail || "")}</small>
    </article>
  `;
}

function renderUsageBars(
  selector,
  rows,
  labelField,
  valueField,
  sortField = valueField,
  sortDirection = "asc",
) {
  const root = $(selector);
  const direction = sortDirection === "desc" ? -1 : 1;
  const sorted = [...rows].sort(
    (a, b) => compareUsageRows(a, b, sortField) * direction,
  );
  if (!sorted.length) {
    root.innerHTML = mutedBlock("No usage in this range.");
    return;
  }
  const max = Math.max(...sorted.map((row) => statValue(row, valueField)), 1);
  root.innerHTML = sorted
    .map((row) => {
      const requests = statValue(row, "requests");
      const width = Math.max(3, Math.round((requests / max) * 100));
      const errors = statValue(row, "errors");
      const totalMs = statDuration(row);
      return `
        <article class="usage-bar-row">
          <div>
            <strong>${escapeHtml(row[labelField] || "-")}</strong>
            <span>${formatCount(requests)} req · ${formatCount(statTotalTokens(row))} tok · ${formatDuration(totalMs)}${errors ? ` · ${formatCount(errors)} err` : ""}</span>
          </div>
          <div class="usage-row-track"><span style="width: ${width}%"></span></div>
        </article>
      `;
    })
    .join("");
}

function renderUsageKeys(rows) {
  const sorted = sortUsageRows(rows, "keys");
  const keyTierMap = {};
  for (const key of state.data.keys || []) {
    if (key.id != null) keyTierMap[key.id] = key.rate_limit_tier || "low";
  }
  $("#usage-keys").innerHTML = sorted.length
    ? usageTable(
        "keys",
        [
          { label: "Client", field: "label", type: "text" },
          { label: "Tier", field: "tier", type: "text" },
          { label: "Req", field: "requests" },
          { label: "Err", field: "errors" },
          { label: "Input", field: "prompt_tokens" },
          { label: "Output", field: "completion_tokens" },
          { label: "Total", field: "total_tokens" },
          { label: "Time", field: "time" },
          { label: "Out tok/s", field: "out_tps" },
        ],
        sorted.map((row) => {
          const cells = usageRowCells(row, row.key_name || `key ${row.key_id ?? "-"}`);
          const tier = row.key_id != null ? keyTierMap[row.key_id] || "-" : "-";
          cells.splice(1, 0, escapeHtml(tier));
          return cells;
        }),
      )
    : empty();
}

function renderUsageModels(rows) {
  const sorted = sortUsageRows(rows, "models");
  $("#usage-models").innerHTML = sorted.length
    ? usageTable(
        "models",
        [
          { label: "Model", field: "label", type: "text" },
          { label: "Req", field: "requests" },
          { label: "Err", field: "errors" },
          { label: "Input", field: "prompt_tokens" },
          { label: "Output", field: "completion_tokens" },
          { label: "Total", field: "total_tokens" },
          { label: "Time", field: "time" },
          { label: "Out tok/s", field: "out_tps" },
        ],
        sorted.map((row) => usageRowCells(row, row.model || "-")),
      )
    : empty();
}

function renderUsageWorkers(rows) {
  const sorted = sortUsageRows(rows, "workers");
  $("#usage-workers").innerHTML = sorted.length
    ? usageTable(
        "workers",
        [
          { label: "Worker", field: "label", type: "text" },
          { label: "Backend", field: "backend", type: "text" },
          { label: "Req", field: "requests" },
          { label: "Err", field: "errors" },
          { label: "Model mix", field: "model_mix", type: "text" },
          { label: "Input", field: "prompt_tokens" },
          { label: "Output", field: "completion_tokens" },
          { label: "Total", field: "total_tokens" },
          { label: "Time", field: "time" },
          { label: "Out tok/s", field: "out_tps" },
        ],
        sorted.map((row) => [
          escapeHtml(row.worker_name || "-"),
          escapeHtml(workerBackendLabel(row.backend || "-")),
          formatCount(statValue(row, "requests")),
          statValue(row, "errors") ? `<span class="error-text">${formatCount(statValue(row, "errors"))}</span>` : "0",
          escapeHtml(workerModelMix(row.worker_name)),
          formatCount(statValue(row, "prompt_tokens")),
          formatCount(statValue(row, "completion_tokens")),
          formatCount(statTotalTokens(row)),
          formatDuration(statDuration(row)),
          outputTokensPerSecond(row),
        ]),
      )
    : empty();
}

function renderUsageCharts(usage) {
  renderTrafficChart(usage.days || []);
  renderWorkerShareChart(usage.workers || [], "#usage-worker-chart", "time");
  renderWorkerShareChart(usage.workers || [], "#usage-request-chart", "requests");
}

function renderTrafficChart(days) {
  const root = $("#usage-traffic-chart");
  if (!days.length) {
    root.innerHTML = mutedBlock("No daily usage in this range.");
    return;
  }
  const ordered = [...days].sort((a, b) => String(a.day).localeCompare(String(b.day)));
  const maxRequests = Math.max(...ordered.map((row) => statValue(row, "requests")), 1);
  root.innerHTML = `
    <div class="mini-bars">
      ${ordered
        .map((row) => {
          const requests = statValue(row, "requests");
          const errors = statValue(row, "errors");
          const height = Math.max(4, Math.round((requests / maxRequests) * 100));
          return `
            <div class="mini-bar-item" title="${escapeAttr(row.day)} · ${formatCount(requests)} req · ${formatCount(errors)} err">
              <div class="mini-bar-track">
                <span class="mini-bar-fill" style="height:${height}%"></span>
                ${errors ? `<span class="mini-bar-error" style="height:${Math.max(3, Math.round((errors / Math.max(requests, 1)) * height))}%"></span>` : ""}
              </div>
              <small>${escapeHtml(shortDate(row.day))}</small>
            </div>
          `;
        })
        .join("")}
    </div>
  `;
}

function renderWorkerShareChart(workers, selector, metric) {
  const root = $(selector);
  const sorted = [...workers]
    .filter((row) => statMetric(row, metric) > 0)
    .sort((a, b) => statMetric(b, metric) - statMetric(a, metric))
    .slice(0, 5);
  const total = workers.reduce((sum, row) => sum + statMetric(row, metric), 0);
  if (!sorted.length || !total) {
    root.innerHTML = mutedBlock("No worker data in this range.");
    return;
  }
  root.innerHTML = `
    <div class="share-list">
      ${sorted
        .map((row) => {
          const value = statMetric(row, metric);
          const percent = Math.round((value / total) * 100);
          const detail = metric === "time"
            ? formatDuration(statDuration(row))
            : `${formatCount(statValue(row, "requests"))} req`;
          return `
            <div class="share-row">
              <div>
                <strong>${escapeHtml(row.worker_name || "-")}</strong>
                <span>${escapeHtml(workerBackendLabel(row.backend || "-"))} · ${detail}</span>
              </div>
              <div class="share-track"><span style="width:${Math.max(2, percent)}%"></span></div>
              <b>${percent}%</b>
            </div>
          `;
        })
        .join("")}
    </div>
  `;
}

function usageRowCells(row, label) {
  return [
    escapeHtml(label),
    formatCount(statValue(row, "requests")),
    statValue(row, "errors") ? `<span class="error-text">${formatCount(statValue(row, "errors"))}</span>` : "0",
    formatCount(statValue(row, "prompt_tokens")),
    formatCount(statValue(row, "completion_tokens")),
    formatCount(statTotalTokens(row)),
    formatDuration(statDuration(row)),
    outputTokensPerSecond(row),
  ];
}

function sortUsageTable(tableName, field) {
  if (!tableName || !field || !state.usageSort[tableName]) return;
  const current = state.usageSort[tableName];
  state.usageSort[tableName] = {
    field,
    direction:
      current.field === field && current.direction === "desc" ? "asc" : "desc",
  };
  renderStats();
}

function sortUsageRows(rows, tableName) {
  const sort = state.usageSort[tableName] || { field: "time", direction: "desc" };
  const direction = sort.direction === "asc" ? 1 : -1;
  return [...rows].sort(
    (a, b) => compareUsageRows(a, b, sort.field) * direction,
  );
}

function compareUsageRows(a, b, field) {
  if (field === "day") {
    return String(a.day || a.usage_day || "").localeCompare(
      String(b.day || b.usage_day || ""),
    );
  }
  if (field === "label") {
    return usageLabel(a).localeCompare(usageLabel(b));
  }
  if (field === "tier") {
    const ta = a.key_id != null ? (keyTierForRow(a) || "") : "";
    const tb = b.key_id != null ? (keyTierForRow(b) || "") : "";
    return ta.localeCompare(tb);
  }
  if (field === "backend") {
    return String(a.backend || "").localeCompare(String(b.backend || ""));
  }
  if (field === "model_mix") {
    return workerModelMix(a.worker_name).localeCompare(workerModelMix(b.worker_name));
  }
  if (field === "time") return statDuration(a) - statDuration(b);
  if (field === "out_tps") return outputTokensPerSecondNumber(a) - outputTokensPerSecondNumber(b);
  if (field === "total_tokens") return statTotalTokens(a) - statTotalTokens(b);
  return statValue(a, field) - statValue(b, field);
}

function keyTierForRow(row) {
  if (row.key_id == null) return null;
  for (const key of state.data.keys || []) {
    if (key.id === row.key_id) return key.rate_limit_tier || "low";
  }
  return null;
}

function usageLabel(row) {
  return row.key_name || row.model || row.worker_name || "";
}

function usageTable(tableName, columns, rows) {
  const sort = state.usageSort[tableName] || {};
  return `
    <table>
      <thead>
        <tr>
          ${columns
            .map((column) => {
              const active = sort.field === column.field;
              const marker = active ? (sort.direction === "asc" ? "▲" : "▼") : "";
              return `<th><button class="sort-button ${active ? "active" : ""}" data-action="sort-usage" data-table="${tableName}" data-field="${column.field}">${escapeHtml(column.label)} <span>${marker}</span></button></th>`;
            })
            .join("")}
        </tr>
      </thead>
      <tbody>${rows.map((row) => `<tr>${row.map((cell) => `<td>${cell}</td>`).join("")}</tr>`).join("")}</tbody>
    </table>
  `;
}

function workerModelMix(workerName) {
  const rows = state.data.usage?.worker_rows || [];
  const models = rows
    .filter((row) => row.worker_name === workerName)
    .sort((a, b) => statValue(b, "requests") - statValue(a, "requests"))
    .slice(0, 3)
    .map((row) => `${row.model} (${formatCount(statValue(row, "request_count"))})`);
  return models.length ? models.join(", ") : "-";
}

function statValue(row, field) {
  if (!row) return 0;
  const aliases = {
    requests: ["requests", "request_count"],
    success: ["success", "success_count"],
    errors: ["errors", "error_count"],
  }[field] || [field];
  for (const alias of aliases) {
    const value = Number(row[alias]);
    if (Number.isFinite(value)) return value;
  }
  return 0;
}

function statTotalTokens(row) {
  return (
    statValue(row, "total_tokens") ||
    statValue(row, "prompt_tokens") + statValue(row, "completion_tokens")
  );
}

function statDuration(row) {
  return statValue(row, "total_ms") || statValue(row, "duration_ms") || statValue(row, "worker_ms");
}

function statMetric(row, metric) {
  if (metric === "time") return statDuration(row);
  if (metric === "requests") return statValue(row, "requests");
  return statValue(row, metric);
}

function outputTokensPerSecond(row) {
  const value = outputTokensPerSecondNumber(row);
  return value == null ? "-" : formatRate(value);
}

function outputTokensPerSecondNumber(row) {
  const seconds = statDuration(row) / 1000;
  if (!seconds) return null;
  return statValue(row, "completion_tokens") / seconds;
}

function shortDate(value) {
  const text = String(value || "");
  return text.length >= 10 ? text.slice(5) : text;
}

async function createKey(event) {
  event.preventDefault();
  const name = $("#new-key-name").value.trim();
  if (!name) return toast("Key name is required.", true);
  const payload = {
    name,
    role: $("#new-key-role").value,
    capture: $("#new-key-capture").checked,
    whitelist_models: splitCsv($("#new-key-whitelist").value),
    blacklist_models: splitCsv($("#new-key-blacklist").value),
    rate_limit_tier: $("#new-key-tier").value,
  };
  const response = await api("management", "/key", {
    method: "POST",
    body: payload,
    expected: [201, 400, 409, 500],
  });
  if (response.status !== 201)
    return toast(`Create failed: ${response.status}`, true);
  $("#created-token").innerHTML =
    `Created token: <code>${escapeHtml(response.body.token)}</code>`;
  $("#create-key-form").reset();
  $("#new-key-capture").checked = true;
  await loadKeys();
}

async function copyToken(token) {
  try {
    await navigator.clipboard.writeText(token);
    toast("Token copied.");
  } catch {
    fallbackCopy(token);
    toast("Token copied.");
  }
}

function fallbackCopy(text) {
  const input = document.createElement("textarea");
  input.value = text;
  input.setAttribute("readonly", "");
  input.style.position = "fixed";
  input.style.opacity = "0";
  document.body.appendChild(input);
  input.select();
  document.execCommand("copy");
  input.remove();
}

async function saveKey(id) {
  const name = document.querySelector(`[data-key-name="${id}"]`).value.trim();
  const capture = document.querySelector(`[data-key-capture="${id}"]`).checked;
  const tier = document.querySelector(`[data-key-tier="${id}"]`).value;
  const response = await api("management", "/key", {
    method: "PATCH",
    body: { id, name, capture, rate_limit_tier: tier },
    expected: [200, 400, 404, 409, 500],
  });
  if (response.status !== 200)
    return toast(`Save failed: ${response.status}`, true);
  toast("Key updated.");
  await loadKeys();
}

async function deleteKey(id) {
  if (!confirm(`Soft delete key ${id}?`)) return;
  const response = await api("management", "/key", {
    method: "DELETE",
    body: { id },
    expected: [204, 404, 500],
  });
  if (response.status !== 204)
    return toast(`Delete failed: ${response.status}`, true);
  toast("Key deleted.");
  await loadKeys();
}

async function sendWorkerCommand(event) {
  event.preventDefault();
  const worker = $("#command-worker").value.trim();
  const command = $("#command-name").value;
  if (!worker) return toast("Worker name is required.", true);

  // Confirmation for destructive commands
  if (command === "SHUTDOWN" || command === "REBOOT") {
    if (!confirm(`Send "${command}" to worker "${worker}"? This will ${command === "SHUTDOWN" ? "shut down" : "reboot"} the worker.`)) {
      return;
    }
  }

  const response = await api("management", "/worker/command", {
    method: "POST",
    body: { worker, command },
    expected: [202, 400, 500],
  });
  if (response.status === 202) {
    toast(`Worker command "${command}" sent to "${worker}".`);
  } else {
    toast(`Command failed: ${response.status}`, true);
  }
}

// ── API Play (inline request/response) ──
// Per-endpoint field schemas: what inputs to show the user
const apiFieldSchemas = {
  // ── Ollama ──
  "/api/generate": [
    { key: "model", label: "Model", type: "text", placeholder: "e.g. llama3.2", default: "llama3.2" },
    { key: "prompt", label: "Prompt", type: "text", placeholder: "e.g. what is the meaning of life?", default: "what is the meaning of life?" },
    { key: "stream", label: "Stream", type: "checkbox", default: false, skipBody: false },
  ],
  "/api/chat": [
    { key: "model", label: "Model", type: "text", placeholder: "e.g. llama3.2", default: "llama3.2" },
    { key: "messages[0].role", label: "Role", type: "text", placeholder: "user / system / assistant", default: "user", skipBodyBuild: "hide" },
    { key: "messages[0].content", label: "Message", type: "text", placeholder: "e.g. what is the meaning of life?", default: "what is the meaning of life?" },
    { key: "stream", label: "Stream", type: "checkbox", default: false, skipBody: false },
  ],
  "/api/embed": [
    { key: "model", label: "Model", type: "text", placeholder: "e.g. all-minilm", default: "all-minilm" },
    { key: "input", label: "Input text", type: "text", placeholder: "e.g. Why is the sky blue?", default: "Why is the sky blue?" },
  ],
  "/api/embeddings": [
    { key: "model", label: "Model", type: "text", placeholder: "e.g. all-minilm", default: "all-minilm" },
    { key: "prompt", label: "Prompt", type: "text", placeholder: "e.g. Here is an article...", default: "Here is an article about llamas..." },
  ],
  "/api/tags": [],
  "/api/ps": [],
  "/api/show": [
    { key: "model", label: "Model", type: "text", placeholder: "e.g. llama3.2", default: "llama3.2" },
  ],
  "/api/version": [],
  "/api/create": [
    { key: "model", label: "New model name", type: "text", placeholder: "e.g. mario", default: "mario" },
    { key: "from", label: "Base model", type: "text", placeholder: "e.g. llama3.2", default: "llama3.2" },
    { key: "system", label: "System prompt", type: "text", placeholder: "e.g. You are Mario...", default: "You are Mario from Super Mario Bros." },
  ],
  "/api/copy": [
    { key: "source", label: "Source model", type: "text", placeholder: "e.g. llama3.2", default: "llama3.2" },
    { key: "destination", label: "Destination name", type: "text", placeholder: "e.g. llama3-backup", default: "llama3-backup" },
  ],
  "/api/pull": [
    { key: "model", label: "Model to pull", type: "text", placeholder: "e.g. llama3.2", default: "llama3.2" },
  ],
  "/api/push": [
    { key: "model", label: "Model to push", type: "text", placeholder: "e.g. my-namespace/my-model:tag", default: "mattw/pygmalion:latest" },
  ],
  "/api/delete": [
    { key: "model", label: "Model to delete", type: "text", placeholder: "e.g. llama3:13b", default: "llama3:13b" },
  ],
  // ── OpenAI / vLLM ──
  "/v1/chat/completions": [
    { key: "model", label: "Model", type: "text", placeholder: "e.g. gpt-4o-mini", default: "gpt-4o-mini" },
    { key: "messages[0].role", label: "Role", type: "text", placeholder: "user / system / assistant", default: "user", skipBodyBuild: "hide" },
    { key: "messages[0].content", label: "Message", type: "text", placeholder: "e.g. what is the meaning of life?", default: "what is the meaning of life?" },
    { key: "stream", label: "Stream", type: "checkbox", default: false, skipBody: false },
    { key: "max_tokens", label: "Max tokens", type: "text", placeholder: "optional", default: "" },
  ],
  "/v1/chat/completions/batch": [
    { key: "model", label: "Model", type: "text", placeholder: "e.g. gpt-4o-mini", default: "gpt-4o-mini" },
    { key: "messages[0].role", label: "Role", type: "text", placeholder: "user", default: "user", skipBodyBuild: "hide" },
    { key: "messages[0].content", label: "Message", type: "text", placeholder: "e.g. what is the meaning of life?", default: "what is the meaning of life?" },
  ],
  "/v1/completions": [
    { key: "model", label: "Model", type: "text", placeholder: "e.g. gpt-3.5-turbo-instruct", default: "gpt-3.5-turbo-instruct" },
    { key: "prompt", label: "Prompt", type: "text", placeholder: "e.g. Once upon a time", default: "Once upon a time" },
    { key: "stream", label: "Stream", type: "checkbox", default: false, skipBody: false },
    { key: "max_tokens", label: "Max tokens", type: "text", placeholder: "optional", default: "" },
  ],
  "/v1/responses": [
    { key: "model", label: "Model", type: "text", placeholder: "e.g. gpt-4o-mini", default: "gpt-4o-mini" },
    { key: "input", label: "Input", type: "text", placeholder: "e.g. what is the meaning of life?", default: "what is the meaning of life?" },
  ],
  "/v1/responses/:response_id": [],
  "/v1/responses/:response_id/cancel": [],
  "/v1/embeddings": [
    { key: "model", label: "Model", type: "text", placeholder: "e.g. text-embedding-3-small", default: "text-embedding-3-small" },
    { key: "input", label: "Input", type: "text", placeholder: "e.g. The quick brown fox", default: "The quick brown fox" },
  ],
  "/v1/models": [],
  "/v1/models/:model": [],
  "/v2/embed": [
    { key: "model", label: "Model", type: "text", placeholder: "e.g. model-name", default: "embed-model" },
    { key: "input", label: "Input", type: "text", placeholder: "e.g. hello world", default: "hello world" },
  ],
  "/score": [
    { key: "model", label: "Model", type: "text", placeholder: "e.g. model-name", default: "score-model" },
    { key: "text", label: "Text", type: "text", placeholder: "e.g. example text", default: "example text" },
  ],
  "/v1/score": [
    { key: "model", label: "Model", type: "text", placeholder: "e.g. model-name", default: "score-model" },
    { key: "text", label: "Text", type: "text", placeholder: "e.g. example text", default: "example text" },
  ],
  "/rerank": [
    { key: "model", label: "Model", type: "text", placeholder: "e.g. rerank-model", default: "rerank-model" },
    { key: "query", label: "Query", type: "text", placeholder: "e.g. What is Python?", default: "What is Python?" },
    { key: "documents", label: "Documents (comma-sep)", type: "text", placeholder: "e.g. Python is a language, Java is a language", default: "Python is a language, Java is a language" },
  ],
  "/v1/rerank": [
    { key: "model", label: "Model", type: "text", placeholder: "e.g. rerank-model", default: "rerank-model" },
    { key: "query", label: "Query", type: "text", placeholder: "e.g. What is Python?", default: "What is Python?" },
    { key: "documents", label: "Documents (comma-sep)", type: "text", placeholder: "e.g. Python is a language, Java is a language", default: "Python is a language, Java is a language" },
  ],
  "/v2/rerank": [
    { key: "model", label: "Model", type: "text", placeholder: "e.g. rerank-model", default: "rerank-model" },
    { key: "query", label: "Query", type: "text", placeholder: "e.g. What is Python?", default: "What is Python?" },
    { key: "documents", label: "Documents (comma-sep)", type: "text", placeholder: "e.g. Python is a language, Java is a language", default: "Python is a language, Java is a language" },
  ],
  "/tokenize": [
    { key: "content", label: "Content", type: "text", placeholder: "e.g. Hello world", default: "Hello world" },
    { key: "model", label: "Model", type: "text", placeholder: "optional", default: "" },
  ],
  "/detokenize": [
    { key: "tokens", label: "Tokens", type: "text", placeholder: "e.g. [1,2,3]", default: "[1,2,3]" },
    { key: "model", label: "Model", type: "text", placeholder: "optional", default: "" },
  ],
  "/health": [],
  "/version": [],
  "/tokenizer_info": [],
  "/is_sleeping": [],
  "/load": [
    { key: "model", label: "Model", type: "text", placeholder: "e.g. model-name", default: "model-name" },
  ],
  "/metrics": [],
  "/v1/load_lora_adapter": [
    { key: "lora_name", label: "LoRA name", type: "text", placeholder: "e.g. my-lora", default: "my-lora" },
    { key: "lora_path", label: "LoRA path", type: "text", placeholder: "e.g. /path/to/lora.safetensors", default: "/path/to/lora.safetensors" },
  ],
  "/v1/unload_lora_adapter": [
    { key: "lora_name", label: "LoRA name", type: "text", placeholder: "e.g. my-lora", default: "my-lora" },
  ],
  "/v1/lora_adapters": [],
  "/start_profile": [],
  "/stop_profile": [],
  "/sleep": [],
  "/wake_up": [],
  // ── Management ──
  "/queue": [],
  "/worker/status": [],
  "/worker/connections": [],
  "/worker/pings": [],
  "/worker/tags": [],
  "/worker/versions": [],
  "/usage?from=YYYY-MM-DD&to=YYYY-MM-DD": [],
  "/key": [],
  "/key/me": [],
  "/key/me/limits": [],
  // Management mutations (POST/PATCH/DELETE)
  "/key#POST": [
    { key: "name", label: "Key name", type: "text", placeholder: "e.g. my-app-key", default: "my-app-key" },
    { key: "capture", label: "Capture", type: "checkbox", default: true, skipBody: false },
    { key: "rate_limit_tier", label: "Rate limit tier", type: "text", placeholder: "0 = default", default: "0" },
  ],
  "/key#PATCH": [
    { key: "id", label: "Key ID", type: "text", placeholder: "e.g. 1", default: "1" },
    { key: "name", label: "Key name", type: "text", placeholder: "optional", default: "" },
    { key: "capture", label: "Capture", type: "checkbox", default: true, skipBody: false },
    { key: "rate_limit_tier", label: "Rate limit tier", type: "text", placeholder: "0 = default", default: "0" },
  ],
  "/key#DELETE": [
    { key: "id", label: "Key ID", type: "text", placeholder: "e.g. 1", default: "1" },
  ],
  "/worker/command#POST": [
    { key: "worker", label: "Worker name", type: "text", placeholder: "e.g. worker-1", default: "worker-1" },
    { key: "command", label: "Command", type: "text", placeholder: "e.g. STATUS, DRAIN, SHUTDOWN, REBOOT", default: "STATUS" },
  ],
};

function openApiPlay(method, path, sectionIndex) {
  const group = apiSurface[Number(sectionIndex)];
  const kind = group.title === "Management" ? "management" : "proxy";
  const modal = $("#api-play-modal");
  const fieldsDiv = $("#api-play-fields");
  const noBody = $("#api-play-no-body");
  const previewPre = $("#api-play-body-preview");
  const sendBtn = $("#api-play-send");
  const responseDiv = $("#api-play-response");
  const statusSpan = $("#api-play-status");
  const bodyPre = $("#api-play-response-body");

  // Set title
  $("#api-play-method").textContent = method;
  $("#api-play-path").textContent = path;
  $("#api-play-method").className = `method ${method.toLowerCase()}`;

  // Look up schema: try method-specific key first, then path-only
  const methodKey = `${path}#${method}`;
  let schemas = apiFieldSchemas[methodKey];
  if (schemas === undefined) schemas = apiFieldSchemas[path];
  if (schemas === undefined) schemas = [];

  // Filter out hidden fields (only used for body building, not shown)
  const visibleSchemas = schemas.filter((f) => f.skipBodyBuild !== "hide");
  const hasBody = method !== "GET";

  // Reset
  responseDiv.style.display = "none";
  statusSpan.textContent = "";
  bodyPre.textContent = "";
  sendBtn.disabled = false;
  sendBtn.textContent = "Send";

  // Render fields or no-body note
  if (!hasBody || visibleSchemas.length === 0) {
    fieldsDiv.innerHTML = "";
    noBody.style.display = "block";
    previewPre.textContent = hasBody ? "{}" : "No body needed.";
  } else {
    noBody.style.display = "none";
    fieldsDiv.innerHTML = visibleSchemas
      .map(
        (f, i) =>
          `<div class="api-play-field">
            <label for="apf-${i}">${escapeHtml(f.label)}</label>
            ${
              f.type === "checkbox"
                ? `<input type="checkbox" id="apf-${i}" data-key="${escapeAttr(f.key)}" ${f.default ? "checked" : ""}>`
                : `<input type="text" id="apf-${i}" data-key="${escapeAttr(f.key)}" placeholder="${escapeAttr(f.placeholder)}" value="${escapeAttr(f.default)}" class="code-textarea" style="resize:none;height:auto;padding:8px 10px;font-size:13px">`
            }
          </div>`,
      )
      .join("");
    // Update preview on any input
    fieldsDiv.querySelectorAll("input").forEach((el) => {
      el.addEventListener("input", () => updateApiPlayPreview());
      el.addEventListener("change", () => updateApiPlayPreview());
    });
    updateApiPlayPreview();
  }

  // Store context on send button
  sendBtn.dataset.kind = kind;
  sendBtn.dataset.method = method;
  sendBtn.dataset.path = path;
  sendBtn.dataset.schemasKey = methodKey;
  sendBtn.dataset.hasBody = hasBody ? "1" : "0";

  // Remove old listener and attach new one
  const newSend = sendBtn.cloneNode(true);
  sendBtn.parentNode.replaceChild(newSend, sendBtn);
  newSend.addEventListener("click", () => sendApiPlay(newSend));

  // Show modal
  modal.style.display = "flex";

  // Close on overlay click
  modal.onclick = (e) => { if (e.target === modal) closeApiPlay(); };
}

function updateApiPlayPreview() {
  const fieldsDiv = $("#api-play-fields");
  const previewPre = $("#api-play-body-preview");
  const inputs = fieldsDiv.querySelectorAll("input");
  const body = {};
  for (const input of inputs) {
    const key = input.dataset.key;
    const val = input.type === "checkbox" ? input.checked : input.value.trim();
    if (val === "" && input.type !== "checkbox") continue;
    // Support dotted paths like "messages[0].content"
    setNested(body, key, val);
  }
  previewPre.textContent = Object.keys(body).length
    ? JSON.stringify(body, null, 2)
    : "{}";
}

function setNested(obj, path, value) {
  const parts = path.split(".");
  let cur = obj;
  for (let i = 0; i < parts.length; i++) {
    const part = parts[i];
    const arrMatch = part.match(/^(\w+)\[(\d+)\]$/);
    if (arrMatch) {
      const arrKey = arrMatch[1];
      const idx = Number(arrMatch[2]);
      if (!cur[arrKey]) cur[arrKey] = [];
      if (i === parts.length - 1) {
        // If value is a comma-separated string for the "documents" field, split into array
        if (typeof value === "string" && (arrKey === "documents" || arrKey === "files")) {
          cur[arrKey][idx] = value.split(",").map((s) => s.trim());
        } else {
          cur[arrKey][idx] = value;
        }
      } else {
        if (!cur[arrKey][idx]) cur[arrKey][idx] = {};
        cur = cur[arrKey][idx];
      }
    } else {
      if (i === parts.length - 1) {
        cur[part] = value;
      } else {
        if (!cur[part]) cur[part] = {};
        cur = cur[part];
      }
    }
  }
}

function closeApiPlay() {
  $("#api-play-modal").style.display = "none";
}

async function sendApiPlay(btn) {
  const method = btn.dataset.method;
  const path = btn.dataset.path;
  const kind = btn.dataset.kind;
  const hasBody = btn.dataset.hasBody === "1";
  const responseDiv = $("#api-play-response");
  const statusSpan = $("#api-play-status");
  const bodyPre = $("#api-play-response-body");

  btn.disabled = true;
  btn.textContent = "Sending...";

  try {
    // Build body from fields - use the schemas to build properly
    let body = null;
    if (hasBody && method !== "GET") {
      const schemasKey = btn.dataset.schemasKey;
      let schemas = apiFieldSchemas[schemasKey];
      if (schemas === undefined) schemas = apiFieldSchemas[path];
      if (schemas === undefined) schemas = [];
      const visibleSchemas = schemas.filter((f) => f.skipBodyBuild !== "hide");

      if (visibleSchemas.length > 0) {
        body = {};
        for (const schema of visibleSchemas) {
          const input = document.querySelector(`#api-play-fields input[data-key="${escapeAttr(schema.key)}"]`);
          if (!input) continue;
          let val = input.type === "checkbox" ? input.checked : input.value.trim();
          if (val === "" && input.type !== "checkbox") continue;

          // Special handling: comma-separated fields ending in "s" that should be arrays
          if (typeof val === "string" && (schema.key === "documents" || schema.key === "files" || schema.key === "images")) {
            val = val.split(",").map((s) => s.trim());
          }
          setNested(body, schema.key, val);
        }
        // Remove empty keys
        for (const key of Object.keys(body)) {
          if (body[key] === "" || body[key] === null) delete body[key];
        }
      }
    }

    const fetchOpts = { method, headers: authHeaders(body ? true : false) };
    if (body && method !== "GET" && method !== "DELETE") {
      fetchOpts.body = JSON.stringify(body);
    }

    const fullPath = apiUrl(kind, path);
    const response = await fetch(fullPath, fetchOpts);
    const text = await response.text();

    statusSpan.textContent = `${response.status} ${response.statusText}`;
    statusSpan.style.color = response.ok ? "var(--accent-strong)" : "var(--danger)";

    let displayText = text;
    try {
      displayText = JSON.stringify(JSON.parse(text), null, 2);
    } catch { /* raw text */ }
    bodyPre.textContent = displayText;
    responseDiv.style.display = "block";

    // Copy handler
    $("#api-play-copy-response").onclick = () => {
      navigator.clipboard.writeText(displayText).then(() => toast("Response copied")).catch(() => {});
    };
  } catch (err) {
    statusSpan.textContent = `Error: ${err.message}`;
    statusSpan.style.color = "var(--danger)";
    bodyPre.textContent = "";
    responseDiv.style.display = "block";
  } finally {
    btn.disabled = false;
    btn.textContent = "Send";
  }
}

function selectModel(source, name, defaultMode) {
  state.selectedModel = {
    source,
    name,
    mode: defaultMode || "chat",
    worker: "",
  };
  state.lastRequest = null;
  const modeSelect = $("#model-console-mode");
  modeSelect.disabled = false;
  modeSelect.value = state.selectedModel.mode;
  $("#model-run-button").disabled = false;
  $("#model-run-button").querySelector(".button-label").textContent = "Run";
  $("#model-mode-hint").textContent = "";
  renderModelConsole();
  renderRequestInfo();
  renderPromptImages();
  setPromptOutput("Response will appear here.", true);
  $("#copy-response-button").style.display = "none";
  $("#clear-response-button").style.display = "none";
}

function renderModelConsole() {
  const selected = state.selectedModel;
  if (!selected) {
    $("#model-console-title").textContent = "Select a model";
    $("#model-console-subtitle").textContent =
      "Choose a model from either list.";
    $("#model-console-mode").disabled = true;
    $("#model-worker-group").classList.add("hidden");
    $("#model-run-button").disabled = true;
    $("#model-mode-hint").textContent = "Select a model first";
    renderRequestInfo();
    return;
  }
  $("#model-console-title").textContent = selected.name;
  $("#model-console-subtitle").textContent =
    selected.source === "ollama"
      ? "Ollama native endpoints"
      : "OpenAI-compatible endpoints";
  $("#model-console-mode").value = selected.mode;
  renderModelWorkerSelect();
  renderPromptImages();
}

function renderModelWorkerSelect() {
  const selected = state.selectedModel;
  const group = $("#model-worker-group");
  const select = $("#model-console-worker");
  if (!selected || !group || !select) {
    group?.classList.add("hidden");
    return;
  }

  const workers = workersForModel(selected);
  const current = workers.includes(selected.worker) ? selected.worker : "";
  selected.worker = current;
  select.innerHTML = [
    `<option value="">Any</option>`,
    ...workers.map(
      (worker) =>
        `<option value="${escapeAttr(worker)}">${escapeHtml(worker)}</option>`,
    ),
  ].join("");
  select.value = current;
  group.classList.remove("hidden");
}

function workersForModel(selected) {
  return Object.entries(state.data.tags || {})
    .filter(([worker, models]) => {
      if (selected.source === "ollama" && workerBackend(worker) === "vllm") {
        return false;
      }
      return (Array.isArray(models) ? models : []).some((model) =>
        modelNamesMatch(workerModelName(model), selected.name),
      );
    })
    .map(([worker]) => worker)
    .sort((a, b) => a.localeCompare(b));
}

function workerBackend(worker) {
  return (
    state.data.connections?.[worker]?.backend ||
    state.data.workers?.[worker]?.backend ||
    state.data.versions?.[worker]?.backend ||
    ""
  );
}

function workerModelName(model) {
  if (typeof model === "string") return model;
  return model?.model || model?.name || model?.id || "";
}

function modelNamesMatch(left, right) {
  if (left === right) return true;
  const bare = (value) =>
    String(value).endsWith(":latest") ? String(value).slice(0, -7) : String(value);
  return bare(left) === bare(right);
}

function beginRequest(selected, path, method, body, input) {
  state.isRunning = true;
  const runBtn = $("#model-run-button");
  runBtn.disabled = true;
  runBtn.querySelector(".button-label").classList.add("hidden");
  runBtn.querySelector(".button-spinner").classList.remove("hidden");

  const bodyText = JSON.stringify(body);
  const images = state.promptImages || [];
  state.lastRequest = {
    id: Date.now(),
    state: "running",
    source: selected.source,
    mode: selected.mode,
    model: selected.name,
    worker: selected.worker || null,
    method,
    path,
    status: null,
    startedAt: new Date(),
    finishedAt: null,
    durationMs: null,
    ttftMs: null,
    promptChars: input.length,
    imageCount: images.length,
    imageBytes: images.reduce((sum, image) => sum + Number(image.size || 0), 0),
    requestBytes: byteLength(bodyText),
    requestBody: sanitizeRequestBodyForDisplay(body),
    responseBytes: 0,
    responseChars: 0,
    chunks: 0,
    streamEvents: 0,
    usage: null,
    backendMetrics: null,
    lastEvent: null,
    responseHeaders: null,
    error: null,
  };
  renderRequestInfo();
}

function finishRun() {
  state.isRunning = false;
  const runBtn = $("#model-run-button");
  runBtn.disabled = false;
  runBtn.querySelector(".button-label").classList.remove("hidden");
  runBtn.querySelector(".button-spinner").classList.add("hidden");
}

function updateRequest(patch) {
  if (!state.lastRequest) return;
  state.lastRequest = { ...state.lastRequest, ...patch };
  if (state.lastRequest.state === "running") {
    state.lastRequest.durationMs = Date.now() - state.lastRequest.startedAt.getTime();
  }
  renderRequestInfo();
}

function finishRequest(result) {
  if (!state.lastRequest || state.lastRequest.state !== "running") return;
  const finishedAt = new Date();
  state.lastRequest = {
    ...state.lastRequest,
    state: result,
    finishedAt,
    durationMs: finishedAt.getTime() - state.lastRequest.startedAt.getTime(),
  };
  renderRequestInfo();
  finishRun();
}

function renderRequestInfo() {
  const request = state.lastRequest;
  const root = $("#request-info");
  const pill = $("#request-state-pill");
  if (!root || !pill) return;
  if (!request) {
    pill.className = "mini-pill";
    pill.textContent = "Idle";
    root.innerHTML = `<div class="list-empty compact-empty">No request yet.</div>`;
    return;
  }

  pill.className = `mini-pill ${request.state}`;
  pill.textContent = titleCase(request.state);
  root.innerHTML = [
    requestSummaryCards(request),
    `<div class="request-card-grid">
      ${requestSection("Route", [
        ["Model", request.model],
        ["Worker", request.worker || "Any"],
        ["Source", request.source],
        ["Mode", request.mode],
        ["Endpoint", `${request.method} ${request.path}`],
        ["HTTP", request.status || "-"],
      ])}
      ${requestSection("Timing", [
      ["Started", request.startedAt.toLocaleTimeString()],
      ["Finished", request.finishedAt ? request.finishedAt.toLocaleTimeString() : "-"],
      ["TTFT", request.ttftMs == null ? "-" : formatMs(request.ttftMs)],
      ["Duration", request.durationMs == null ? "-" : formatMs(request.durationMs)],
      ])}
      ${requestSection("Payload", [
        ["Prompt", `${request.promptChars.toLocaleString()} chars`],
        ["Images", request.imageCount ? `${request.imageCount} (${formatBytes(request.imageBytes)})` : "0"],
        ["Request", formatBytes(request.requestBytes)],
        ["Response", formatBytes(request.responseBytes)],
        ["Output", `${request.responseChars.toLocaleString()} chars`],
        ["Chunks", request.chunks],
        ["Events", request.streamEvents],
      ])}
      ${requestThroughput(request)}
    </div>`,
    request.usage ? requestUsage(request.usage) : "",
    requestJsonSection("Request body", request.requestBody, false),
    request.backendMetrics
      ? requestJsonSection("Backend", request.backendMetrics, false)
      : "",
    request.responseHeaders
      ? requestJsonSection("Response headers", request.responseHeaders, false)
      : "",
    request.error ? requestSection("Error", [["Message", request.error]]) : "",
    request.lastEvent ? requestJsonSection("Last event", request.lastEvent, false) : "",
  ].join("");
}

function requestSummaryCards(request) {
  const tokens = tokenBreakdown(request).generated ?? "-";
  const rates = requestRates(request);
  return `
    <section class="request-summary">
      <article class="${request.status && request.status < 400 ? "good" : request.status ? "bad" : ""}">
        <span>Status</span>
        <strong>${escapeHtml(String(request.status || "-"))}</strong>
      </article>
      <article class="${request.durationMs != null && request.durationMs < 1000 ? "good" : ""}">
        <span>Duration</span>
        <strong>${escapeHtml(request.durationMs == null ? "-" : formatMs(request.durationMs))}</strong>
      </article>
      <article class="${request.ttftMs != null && request.ttftMs < 1000 ? "good" : ""}">
        <span>TTFT</span>
        <strong>${escapeHtml(request.ttftMs == null ? "-" : formatMs(request.ttftMs))}</strong>
      </article>
      <article>
        <span>Response</span>
        <strong>${escapeHtml(formatBytes(request.responseBytes))}</strong>
      </article>
      <article>
        <span>Tokens</span>
        <strong>${escapeHtml(String(tokens))}</strong>
      </article>
      <article>
        <span>Gen tok/s</span>
        <strong>${escapeHtml(rates.generatedTokensPerSecond ?? "-")}</strong>
      </article>
    </section>
  `;
}

function requestRates(request) {
  const durationSeconds = request.durationMs ? request.durationMs / 1000 : 0;
  const tokens = tokenBreakdown(request);
  const evalSeconds = request.backendMetrics?.eval_duration_ns
    ? request.backendMetrics.eval_duration_ns / 1_000_000_000
    : null;
  const generatedSeconds = evalSeconds || durationSeconds;
  const visibleSeconds =
    request.durationMs && request.ttftMs != null
      ? Math.max((request.durationMs - request.ttftMs) / 1000, 0.001)
      : durationSeconds;
  return {
    generatedTokensPerSecond:
      tokens.generated == null || !generatedSeconds
        ? null
        : formatRate(tokens.generated / generatedSeconds),
    visibleTokensPerSecond:
      tokens.visible == null || !visibleSeconds
        ? null
        : formatRate(tokens.visible / visibleSeconds),
    totalTokensPerSecond:
      tokens.total == null || !durationSeconds
        ? null
        : formatRate(tokens.total / durationSeconds),
    charsPerSecond: durationSeconds
      ? formatRate(request.responseChars / durationSeconds)
      : null,
    bytesPerSecond: durationSeconds
      ? `${formatRate(request.responseBytes / durationSeconds)} B/s`
      : null,
  };
}

function tokenBreakdown(request) {
  const usage = request.usage || {};
  const prompt = usageNumber(usage, [
    "prompt_tokens",
    "promptTokens",
    "input_tokens",
    "inputTokens",
  ]);
  const completion = usageNumber(usage, [
    "completion_tokens",
    "completionTokens",
    "output_tokens",
    "outputTokens",
  ]);
  const total = usageNumber(usage, ["total_tokens", "totalTokens"]);
  const backendGenerated = request.backendMetrics?.eval_count ?? null;
  const generated =
    total != null && prompt != null
      ? Math.max(total - prompt, completion ?? 0)
      : completion == null
        ? backendGenerated
        : completion;
  return { prompt, completion, generated, visible: completion, total };
}

function usageNumber(value, names) {
  if (!value || typeof value !== "object") return null;
  for (const name of names) {
    const number = Number(value[name]);
    if (Number.isFinite(number)) return number;
  }
  return null;
}

function requestUsage(usage) {
  const tokens = tokenBreakdown({ usage });
  const prompt = tokens.prompt ?? 0;
  const completion = tokens.completion ?? 0;
  const total = tokens.total ?? prompt + completion;
  const promptPercent = total ? Math.round((prompt / total) * 100) : 0;
  const completionPercent = total ? Math.round((completion / total) * 100) : 0;
  return `
    <section class="usage-card">
      <div class="usage-head">
        <h4>Token Usage</h4>
        <strong>${escapeHtml(String(total || "-"))}</strong>
      </div>
      <div class="usage-bar" title="${prompt} prompt / ${completion} completion">
        <span class="prompt" style="width: ${promptPercent}%"></span>
        <span class="completion" style="width: ${completionPercent}%"></span>
      </div>
      <div class="usage-grid">
        <div>
          <span>Prompt</span>
          <strong>${escapeHtml(String(prompt || "-"))}</strong>
        </div>
        <div>
          <span>Completion</span>
          <strong>${escapeHtml(String(completion || "-"))}</strong>
        </div>
        <div>
          <span>Total</span>
          <strong>${escapeHtml(String(total || "-"))}</strong>
        </div>
      </div>
    </section>
  `;
}

function requestThroughput(request) {
  const rates = requestRates(request);
  const source = request.backendMetrics?.eval_duration_ns
    ? "backend eval"
    : "end-to-end";
  return requestSection("Throughput", [
    ["Generated tok/s", rates.generatedTokensPerSecond ?? "-"],
    ["Visible tok/s", rates.visibleTokensPerSecond ?? "-"],
    ["Total tok/s", rates.totalTokensPerSecond ?? "-"],
    ["Chars/s", rates.charsPerSecond ?? "-"],
    ["Bytes/s", rates.bytesPerSecond ?? "-"],
    ["Basis", source],
  ]);
}

function requestSection(title, rows) {
  return `
    <section class="request-section">
      <h4>${escapeHtml(title)}</h4>
      ${rows
        .map(
          ([label, value]) => `
            <div class="request-row">
              <span>${escapeHtml(label)}</span>
              <strong>${escapeHtml(String(value ?? "-"))}</strong>
            </div>
          `,
        )
        .join("")}
    </section>
  `;
}

function requestJsonSection(title, value, open = false) {
  return `
    <details class="request-details" ${open ? "open" : ""}>
      <summary>${escapeHtml(title)}</summary>
      <pre class="request-json">${escapeHtml(pretty(value))}</pre>
    </details>
  `;
}

function sanitizeRequestBodyForDisplay(value) {
  if (Array.isArray(value)) return value.map(sanitizeRequestBodyForDisplay);
  if (!value || typeof value !== "object") return value;
  const result = {};
  for (const [key, child] of Object.entries(value)) {
    if (key === "images" && Array.isArray(child)) {
      result[key] = child.map((image, index) => `[image ${index + 1}, base64 omitted]`);
    } else if (key === "image_url" && child?.url) {
      result[key] = { ...child, url: "[image data URL omitted]" };
    } else {
      result[key] = sanitizeRequestBodyForDisplay(child);
    }
  }
  return result;
}

async function handlePromptImages(event) {
  const files = Array.from(event.target.files || []);
  try {
    const images = await Promise.all(files.map(readPromptImage));
    state.promptImages = [...state.promptImages, ...images];
    event.target.value = "";
    renderPromptImages();
  } catch (error) {
    toast(`Image upload failed: ${error.message || error}`, true);
  }
}

function readPromptImage(file) {
  return new Promise((resolve, reject) => {
    const reader = new FileReader();
    reader.onerror = () => reject(reader.error || new Error("image read failed"));
    reader.onload = () => {
      const dataUrl = String(reader.result || "");
      const base64 = dataUrl.split(",", 2)[1] || "";
      resolve({
        id: `${Date.now()}-${Math.random().toString(16).slice(2)}`,
        name: file.name,
        type: file.type || "image/*",
        size: file.size,
        dataUrl,
        base64,
      });
    };
    reader.readAsDataURL(file);
  });
}

function removePromptImage(id) {
  state.promptImages = state.promptImages.filter((image) => image.id !== id);
  renderPromptImages();
}

function renderPromptImages() {
  const root = $("#prompt-image-list");
  if (!root) return;
  const disabled = state.selectedModel?.mode === "embedding";
  const inputDisabled = Boolean(disabled || !state.selectedModel);
  $("#prompt-images").disabled = inputDisabled;
  $("#prompt-image-button").disabled = inputDisabled;
  if (!state.promptImages.length) {
    root.innerHTML = `<span class="image-empty">${disabled ? "Images are only available in chat mode." : "No images attached."}</span>`;
    return;
  }
  root.innerHTML = state.promptImages
    .map(
      (image) => `
        <article class="image-preview">
          <img src="${escapeAttr(image.dataUrl)}" alt="${escapeAttr(image.name)}" />
          <div>
            <strong>${escapeHtml(image.name || "image")}</strong>
            <span>${escapeHtml(image.type)} · ${formatBytes(image.size)}</span>
          </div>
          <button class="icon-button" type="button" data-action="remove-prompt-image" data-id="${escapeAttr(image.id)}" title="Remove image">x</button>
        </article>
      `,
    )
    .join("");
}

async function runPrompt(event) {
  event.preventDefault();
  if (state.isRunning) return;
  const selected = state.selectedModel;
  const input = $("#prompt-text").value;
  if (!selected) return toast("Select a model first.", true);
  if (selected.mode === "embedding" && state.promptImages.length) {
    return toast("Images can only be sent in chat mode.", true);
  }
  setPromptOutput("Running...", true);
  setStatus(`Running ${selected.mode} on ${selected.name}...`);
  $("#copy-response-button").style.display = "none";
  $("#clear-response-button").style.display = "none";

  if (selected.mode === "embedding") {
    await runEmbedding(selected, input);
    setStatus("Embedding finished.");
    return;
  }

  if (selected.source === "openai") {
    const body = {
      model: selected.name,
      messages: [openAiChatMessage(input, state.promptImages)],
      stream: true,
    };
    beginRequest(selected, "/v1/chat/completions", "POST", body, input);
    await streamRequest(
      "/v1/chat/completions",
      body,
      parseOpenAiStream,
      selected.worker,
    );
  } else {
    const body = {
      model: selected.name,
      messages: [ollamaChatMessage(input, state.promptImages)],
      stream: true,
    };
    beginRequest(selected, "/api/chat", "POST", body, input);
    await streamRequest(
      "/api/chat",
      body,
      parseOllamaChatStream,
      selected.worker,
    );
  }
  finishRequest("done");
  setStatus("Prompt finished.");
  // Show copy/clear buttons
  $("#copy-response-button").style.display = "";
  $("#clear-response-button").style.display = "";
}

function openAiChatMessage(text, images) {
  if (!images.length) return { role: "user", content: text };
  return {
    role: "user",
    content: [
      { type: "text", text },
      ...images.map((image) => ({
        type: "image_url",
        image_url: { url: image.dataUrl },
      })),
    ],
  };
}

function ollamaChatMessage(text, images) {
  const message = { role: "user", content: text };
  if (images.length) {
    message.images = images.map((image) => image.base64);
  }
  return message;
}

async function runEmbedding(selected, input) {
  const path = selected.source === "openai" ? "/v1/embeddings" : "/api/embed";
  const body =
    selected.source === "openai"
      ? { model: selected.name, input }
      : { model: selected.name, input };
  beginRequest(selected, path, "POST", body, input);
  const response = await api("proxy", path, {
    method: "POST",
    body,
    expected: [200, 400, 403, 404, 500],
    node: selected.worker,
  });
  updateRequest({
    status: response.status,
    responseBytes: byteLength(JSON.stringify(response.body ?? "")),
    responseChars: JSON.stringify(response.body ?? "").length,
    usage: response.body?.usage || null,
    backendMetrics: embeddingMetrics(response.body),
  });
  finishRequest(response.status === 200 ? "done" : "error");
  setPromptOutput(summarizeEmbeddingResponse(response.status, response.body));
  // Show copy/clear buttons
  $("#copy-response-button").style.display = "";
  $("#clear-response-button").style.display = "";
}

async function streamRequest(path, body, parser, node = "") {
  try {
    const response = await fetch(apiUrl("proxy", path), {
      method: "POST",
      headers: authHeaders(true, node),
      body: JSON.stringify(body),
    });
    updateRequest({
      status: response.status,
      responseHeaders: headersObject(response.headers),
    });
    if (!response.ok || !response.body) {
      const text = await response.text();
      updateRequest({
        error: text || `HTTP ${response.status}`,
        responseBytes: byteLength(text),
        responseChars: text.length,
      });
      finishRequest("error");
      setPromptOutput(`HTTP ${response.status}\n${text}`);
      return;
    }

    const reader = response.body.getReader();
    const decoder = new TextDecoder();
    let buffer = "";
    while (true) {
      const { done, value } = await reader.read();
      if (done) break;
      buffer += decoder.decode(value, { stream: true });
      const parsed = parser(buffer);
      buffer = parsed.rest;
      const hasOutput = Boolean(parsed.text);
      updateRequest({
        ttftMs:
          hasOutput && state.lastRequest.ttftMs == null
            ? Date.now() - state.lastRequest.startedAt.getTime()
            : state.lastRequest.ttftMs,
        chunks: state.lastRequest.chunks + 1,
        responseBytes: state.lastRequest.responseBytes + value.byteLength,
        responseChars: state.lastRequest.responseChars + (parsed.text || "").length,
        streamEvents: state.lastRequest.streamEvents + (parsed.events || 0),
        usage: parsed.usage || state.lastRequest.usage,
        backendMetrics: parsed.metrics || state.lastRequest.backendMetrics,
        lastEvent: parsed.lastEvent || state.lastRequest.lastEvent,
      });
      if (hasOutput) appendPromptOutput(parsed.text);
    }
  } catch (error) {
    updateRequest({ error: error.message || String(error) });
    finishRequest("error");
    setPromptOutput(error.message || String(error));
  }
}

function parseOllamaStream(buffer) {
  const lines = buffer.split(/\r?\n/);
  const rest = lines.pop() || "";
  let text = "";
  let events = 0;
  let lastEvent = null;
  let metrics = null;
  for (const line of lines) {
    if (!line.trim()) continue;
    try {
      const parsed = JSON.parse(line);
      events += 1;
      lastEvent = compactEvent(parsed);
      text += parsed.response || "";
      if (parsed.done) metrics = ollamaMetrics(parsed);
    } catch {
      text += `${line}\n`;
    }
  }
  return { text, rest, events, lastEvent, metrics };
}

function parseOllamaChatStream(buffer) {
  const lines = buffer.split(/\r?\n/);
  const rest = lines.pop() || "";
  let text = "";
  let events = 0;
  let lastEvent = null;
  let metrics = null;
  for (const line of lines) {
    if (!line.trim()) continue;
    try {
      const parsed = JSON.parse(line);
      events += 1;
      lastEvent = compactEvent(parsed);
      text += parsed.message?.content || parsed.response || "";
      if (parsed.done) metrics = ollamaMetrics(parsed);
    } catch {
      text += `${line}\n`;
    }
  }
  return { text, rest, events, lastEvent, metrics };
}

function parseOpenAiStream(buffer) {
  const lines = buffer.split(/\r?\n/);
  const rest = lines.pop() || "";
  let text = "";
  let events = 0;
  let usage = null;
  let lastEvent = null;
  for (const line of lines) {
    const trimmed = line.trim();
    if (!trimmed.startsWith("data:")) continue;
    const data = trimmed.slice(5).trim();
    if (!data || data === "[DONE]") continue;
    try {
      const parsed = JSON.parse(data);
      events += 1;
      lastEvent = compactEvent(parsed);
      if (parsed.usage) usage = parsed.usage;
      text +=
        parsed.choices?.[0]?.delta?.content || parsed.choices?.[0]?.text || "";
    } catch {
      text += `${data}\n`;
    }
  }
  return { text, rest, events, usage, lastEvent };
}

function compactEvent(value) {
  if (!value || typeof value !== "object") return value;
  const clone = { ...value };
  delete clone.context;
  delete clone.embedding;
  delete clone.embeddings;
  delete clone.data;
  return clone;
}

function ollamaMetrics(value) {
  return {
    done_reason: value.done_reason || null,
    total_duration_ns: value.total_duration ?? null,
    load_duration_ns: value.load_duration ?? null,
    prompt_eval_count: value.prompt_eval_count ?? null,
    prompt_eval_duration_ns: value.prompt_eval_duration ?? null,
    eval_count: value.eval_count ?? null,
    eval_duration_ns: value.eval_duration ?? null,
  };
}

function summarizeEmbeddingResponse(status, body) {
  if (status !== 200) {
    return `HTTP ${status}\n${pretty(body)}`;
  }
  const vectors = [];
  collectEmbeddingVectors(body, vectors);
  const summary = {
    status,
    vectors: vectors.length,
    dimensions: vectors
      .map((vector) => vector.length)
      .filter(Boolean)
      .slice(0, 10),
    usage: body?.usage || null,
  };
  return pretty(summary);
}

function embeddingMetrics(body) {
  const vectors = [];
  collectEmbeddingVectors(body, vectors);
  return {
    vectors: vectors.length,
    dimensions: vectors
      .map((vector) => vector.length)
      .filter(Boolean)
      .slice(0, 10),
    usage: body?.usage || null,
  };
}

function collectEmbeddingVectors(value, vectors) {
  if (!value || typeof value !== "object") return;
  if (Array.isArray(value) && value.every((item) => typeof item === "number")) {
    vectors.push(value);
    return;
  }
  if (Array.isArray(value)) {
    value.forEach((item) => collectEmbeddingVectors(item, vectors));
    return;
  }
  for (const child of Object.values(value)) {
    collectEmbeddingVectors(child, vectors);
  }
}

async function api(kind, path, options = {}) {
  const method = options.method || "GET";
  const response = await fetch(apiUrl(kind, path), {
    method,
    headers: authHeaders(Boolean(options.body), options.node),
    body: options.body ? JSON.stringify(options.body) : undefined,
  });
  const text = await response.text();
  const body = parseBody(text);
  const expected = options.expected || [200];
  if (!expected.includes(response.status)) {
    throw new Error(
      `${method} ${kind}${path} returned ${response.status}: ${text}`,
    );
  }
  return { status: response.status, body };
}

function apiUrl(kind, path) {
  if (kind === "proxy") {
    const endpoint = state.config?.proxyEndpoint;
    if (!endpoint) throw new Error("HiveCore proxy endpoint is not configured");
    return `${endpoint.replace(/\/+$/, "")}${path}`;
  }
  return `/api/${kind}${path}`;
}

async function requestJson(path) {
  const response = await fetch(path);
  if (!response.ok) throw new Error(`${path} returned ${response.status}`);
  return response.json();
}

function authHeaders(hasBody = true, node = "") {
  const headers = {};
  if (hasBody) headers["content-type"] = "application/json";
  if (state.key) {
    headers.authorization = `Bearer ${state.key}`;
    headers["api-key"] = state.key;
  }
  if (node) headers.node = node;
  return headers;
}

function parseBody(text) {
  if (!text) return null;
  try {
    return JSON.parse(text);
  } catch {
    return text;
  }
}

function extractOllamaModels(value) {
  if (!value || value.error) return [];
  if (Array.isArray(value.models)) return value.models;
  if (Array.isArray(value)) return value;
  return [];
}

function extractOpenAiModels(value) {
  if (!value || value.error) return [];
  if (Array.isArray(value.data)) return value.data;
  if (Array.isArray(value.models)) return value.models;
  if (Array.isArray(value)) return value;
  return [];
}

function defaultModeForModel(model) {
  const capabilities = Array.isArray(model.capabilities)
    ? model.capabilities
    : [];
  const family = model.details?.family || "";
  const name = model.name || model.model || model.id || "";
  if (
    capabilities.includes("embedding") ||
    family.toLowerCase().includes("bert") ||
    name.toLowerCase().includes("embed") ||
    name.toLowerCase().includes("bge")
  ) {
    return "embedding";
  }
  return "chat";
}

function renderApiSurface() {
  $("#api-surface").innerHTML = apiSurface
    .map((group) => ({
      ...group,
      endpoints: group.endpoints.filter((endpoint) =>
        endpointVisible(group.title, endpoint),
      ),
    }))
    .filter((group) => group.endpoints.length > 0)
    .map(
      (group, gi) => `
        <section class="api-card">
          <h3>${escapeHtml(group.title)}</h3>
          <div class="endpoint-list">
            ${group.endpoints
              .map(
                ([method, path, scope]) => `
                  <div class="endpoint" data-action="api-endpoint" data-method="${escapeAttr(method)}" data-path="${escapeAttr(path)}" data-section="${gi}" title="Click to focus endpoint">
                    <span class="method ${method.toLowerCase()}">${method}</span>
                    <div>
                      <div class="path">${escapeHtml(path)}</div>
                      <div class="scope">${escapeHtml(scope)}</div>
                    </div>
                    <button class="api-play-btn" data-action="api-play" data-section="${gi}" data-method="${escapeAttr(method)}" data-path="${escapeAttr(path)}" title="Play endpoint">&#9654;</button>
                  </div>
                `,
              )
              .join("")}
          </div>
        </section>
      `,
    )
    .join("");
}

function endpointVisible(groupTitle, endpoint) {
  const [method, path, scope] = endpoint;
  if (scope === "client") return true;
  if (state.admin) return true;
  if (!state.managementRead || groupTitle !== "Management") return false;
  return method === "GET" && [
    "/queue",
    "/worker/status",
    "/worker/connections",
    "/worker/pings",
    "/worker/tags",
    "/worker/versions",
    "/usage?from=YYYY-MM-DD&to=YYYY-MM-DD",
    "/key",
  ].includes(path);
}

function table(headers, rows) {
  return `
    <table>
      <thead><tr>${headers.map((header) => `<th>${escapeHtml(header)}</th>`).join("")}</tr></thead>
      <tbody>${rows.map((row) => `<tr>${row.map((cell) => `<td>${cell}</td>`).join("")}</tr>`).join("")}</tbody>
    </table>
  `;
}

function tags(values) {
  const list = Array.isArray(values) ? values : [];
  if (!list.length) return '<span class="tag">any</span>';
  return `<div class="tag-list">${list.map((value) => `<span class="tag">${escapeHtml(String(value))}</span>`).join("")}</div>`;
}

function empty() {
  return $("#empty-template").innerHTML;
}

function splitCsv(value) {
  return value
    .split(",")
    .map((item) => item.trim())
    .filter(Boolean);
}

function pretty(value) {
  return JSON.stringify(value, null, 2);
}

function titleCase(value) {
  return value.slice(0, 1).toUpperCase() + value.slice(1);
}

function formatMs(value) {
  if (value === undefined || value === null) return "-";
  if (value < 1000) return `${value}ms`;
  return `${Math.round(value / 1000)}s`;
}

function formatBytes(value) {
  const number = Number(value);
  if (!Number.isFinite(number)) return "-";
  const units = ["B", "KB", "MB", "GB", "TB"];
  let size = number;
  let unit = 0;
  while (size >= 1024 && unit < units.length - 1) {
    size /= 1024;
    unit += 1;
  }
  return `${size.toFixed(size >= 10 || unit === 0 ? 0 : 1)} ${units[unit]}`;
}

function formatRate(value) {
  const number = Number(value);
  if (!Number.isFinite(number)) return "-";
  if (number >= 100) return number.toFixed(0);
  if (number >= 10) return number.toFixed(1);
  return number.toFixed(2);
}

function formatCount(value) {
  const number = Number(value);
  if (!Number.isFinite(number)) return "-";
  const abs = Math.abs(number);
  if (abs >= 1_000_000_000) return `${formatRate(number / 1_000_000_000)}B`;
  if (abs >= 1_000_000) return `${formatRate(number / 1_000_000)}M`;
  if (abs >= 1_000) return `${formatRate(number / 1_000)}k`;
  return Math.round(number).toLocaleString();
}

function formatDuration(value) {
  const ms = Number(value);
  if (!Number.isFinite(ms) || ms <= 0) return "-";
  const seconds = Math.round(ms / 1000);
  if (seconds < 1) return `${Math.round(ms)}ms`;
  if (seconds < 60) return `${seconds}s`;
  const minutes = Math.floor(seconds / 60);
  const restSeconds = seconds % 60;
  if (minutes < 60) return `${minutes}m ${String(restSeconds).padStart(2, "0")}s`;
  const hours = Math.floor(minutes / 60);
  const restMinutes = minutes % 60;
  return `${hours}h ${String(restMinutes).padStart(2, "0")}m`;
}

function localDateString(date) {
  const year = date.getFullYear();
  const month = String(date.getMonth() + 1).padStart(2, "0");
  const day = String(date.getDate()).padStart(2, "0");
  return `${year}-${month}-${day}`;
}

function byteLength(value) {
  return new TextEncoder().encode(String(value ?? "")).length;
}

function headersObject(headers) {
  const result = {};
  headers.forEach((value, key) => {
    result[key] = value;
  });
  return result;
}

function maskToken(token) {
  if (!token) return "-";
  if (token.length <= 12) return token;
  return `${token.slice(0, 8)}...${token.slice(-6)}`;
}

function stringifyError(value) {
  return typeof value === "string" ? value : JSON.stringify(value);
}

// ── Toast system ──
function toast(message, error = false) {
  const container = $("#toast-container");
  const el = document.createElement("div");
  el.className = `toast${error ? " error" : ""}`;
  el.innerHTML = `<span>${escapeHtml(message)}</span><button type="button" class="toast-close">✕</button>`;
  container.appendChild(el);

  el.querySelector(".toast-close").addEventListener("click", () => dismissToast(el));

  // Auto-dismiss after 4s
  setTimeout(() => dismissToast(el), 4000);
}

function dismissToast(el) {
  if (el.classList.contains("dismissing")) return;
  el.classList.add("dismissing");
  setTimeout(() => el.remove(), 200);
}

function setStatus(message, error = false) {
  const element = $("#status-line");
  element.textContent = message;
  element.classList.toggle("error", error);
}

function setPromptOutput(text, empty = false) {
  const element = $("#prompt-output");
  element.textContent = text;
  element.classList.toggle("empty", empty);
}

function appendPromptOutput(text) {
  const element = $("#prompt-output");
  if (element.classList.contains("empty")) {
    element.textContent = "";
    element.classList.remove("empty");
  }
  element.textContent += text;
  element.scrollTop = element.scrollHeight;
}

function setLoginStatus(message, error = false) {
  const element = $("#login-status-line");
  element.textContent = message;
  element.classList.toggle("error", error);
}

function escapeHtml(value) {
  return String(value)
    .replaceAll("&", "&amp;")
    .replaceAll("<", "&lt;")
    .replaceAll(">", "&gt;")
    .replaceAll('"', "&quot;");
}

function escapeAttr(value) {
  return escapeHtml(value).replaceAll("'", "&#39;");
}
