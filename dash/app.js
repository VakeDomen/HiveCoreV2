const state = {
  config: null,
  key: "",
  role: "disconnected",
  admin: false,
  selectedModel: null,
  lastRequest: null,
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
  state.config = await requestJson("/config.json", { noAuth: true });
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
    state.key = $("#key-input").value.trim();
    await connect();
  });

  $("#logout-button").addEventListener("click", () => {
    if (state.refreshTimer) {
      clearInterval(state.refreshTimer);
      state.refreshTimer = null;
    }
    $("#dashboard-shell").classList.add("hidden");
    $("#login-screen").classList.remove("hidden");
    state.role = "disconnected";
    state.admin = false;
    renderRole();
    setLoginStatus("Enter a key to connect.");
  });

  $("#toggle-key").addEventListener("click", () => {
    const input = $("#key-input");
    input.type = input.type === "password" ? "text" : "password";
  });

  $("#refresh-button").addEventListener("click", () => refreshCurrentView());
  $("#refresh-interval").addEventListener("change", () => {
    state.refreshIntervalMs = Number($("#refresh-interval").value);
    configureAutoRefresh();
  });

  $$(".nav-button").forEach((button) => {
    button.addEventListener("click", () => showView(button.dataset.view));
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
    if (action === "delete-key")
      return deleteKey(Number(actionTarget.dataset.id));
    if (action === "save-key") return saveKey(Number(actionTarget.dataset.id));
    if (action === "copy-token") return copyToken(actionTarget.dataset.token);
    if (action === "select-model") {
      return selectModel(
        actionTarget.dataset.source,
        actionTarget.dataset.model,
        actionTarget.dataset.defaultMode,
      );
    }
  });

  $("#create-key-form").addEventListener("submit", createKey);
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
  $("#model-console-mode").addEventListener("change", () => {
    if (state.selectedModel) {
      state.selectedModel.mode = $("#model-console-mode").value;
      renderModelConsole();
    }
  });
  $$("#key-role-tabs .tab").forEach((button) => {
    button.addEventListener("click", () => {
      state.keyRoleFilter = button.dataset.roleFilter;
      renderKeys();
    });
  });
}

async function connect() {
  if (!state.key) {
    setLoginStatus("Enter a Hive key.", true);
    return;
  }
  setLoginStatus("Checking key...");
  resetData();

  const adminProbe = await api("management", "/key", {
    expected: [200, 403, 401],
  });
  if (adminProbe.status === 200) {
    state.admin = true;
    state.role = "admin";
    state.refreshIntervalMs = Number($("#refresh-interval").value || 5000);
    state.data.keys = adminProbe.body;
    setStatus("Connected as admin.");
    await Promise.all([loadWorkers(), loadQueue(), loadOllamaTags(), loadOpenAiModels()]);
  } else {
    const modelProbe = await api("proxy", "/api/tags", {
      expected: [200, 401, 403, 404, 500],
    });
    if (modelProbe.status === 401 || modelProbe.status === 403) {
      state.admin = false;
      state.role = "disconnected";
      setLoginStatus("Key was rejected by HiveCore.", true);
      renderRole();
      return;
    }
    state.admin = false;
    state.role = "client";
    state.refreshIntervalMs = 0;
    $("#refresh-interval").value = "0";
    setStatus("Connected as client.");
    if (modelProbe.status === 200) state.data.ollamaTags = modelProbe.body;
    await loadOpenAiModels({ quiet: true });
  }

  renderRole();
  renderAll();
  $("#login-screen").classList.add("hidden");
  $("#dashboard-shell").classList.remove("hidden");
  configureAutoRefresh();
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
  };
  state.selectedModel = null;
  state.lastRequest = null;
}

function renderRole() {
  const pill = $("#role-pill");
  pill.className = `pill ${state.role === "admin" ? "admin" : state.role === "client" ? "client" : ""}`;
  pill.textContent =
    state.role === "admin"
      ? "Admin key"
      : state.role === "client"
        ? "Client key"
        : "Disconnected";
  $("#metric-role").textContent = state.role;
  $(".refresh-control").classList.toggle("hidden", !state.admin);
  $$(".admin-only").forEach((element) => {
    element.classList.toggle("hidden", !state.admin);
  });
  renderApiSurface();
  if (!state.admin && ["keys", "workers", "stats"].includes(currentView())) {
    showView("overview");
  }
}

function configureAutoRefresh() {
  if (state.refreshTimer) {
    clearInterval(state.refreshTimer);
    state.refreshTimer = null;
  }
  if (!state.admin || !state.refreshIntervalMs) return;
  state.refreshTimer = setInterval(() => {
    refreshCurrentView({ soft: true }).catch((error) => {
      setStatus(error.message, true);
    });
  }, state.refreshIntervalMs);
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
  if (!state.key) return;
  const view = currentView();
  if (!options.soft) setStatus("Refreshing...");
  if (view === "overview") {
    await Promise.all([
      state.admin ? loadWorkers({ quiet: true }) : Promise.resolve(),
      state.admin ? loadQueue({ quiet: true }) : Promise.resolve(),
      loadOllamaTags({ quiet: true }),
      loadOpenAiModels({ quiet: true }),
    ]);
  }
  if (view === "models")
    await Promise.all([
      loadOllamaTags({ quiet: true }),
      loadOpenAiModels({ quiet: true }),
    ]);
  if (view === "keys" && state.admin) await loadKeys({ quiet: true });
  if (view === "workers" && state.admin)
    await Promise.all([
      loadWorkers({ quiet: true }),
      loadQueue({ quiet: true }),
    ]);
  if (view === "stats" && state.admin) await loadUsage({ quiet: true });
  renderAll();
  if (!options.soft) setStatus("Refreshed.");
}

async function loadKeys() {
  const response = await api("management", "/key");
  state.data.keys = response.body;
  renderKeys();
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
}

async function loadQueue() {
  const response = await api("management", "/queue");
  state.data.queue = response.body;
  renderQueue();
}

async function loadUsage() {
  if (!state.admin) return;
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
    expected: [200, 404, 500],
  });
  state.data.ollamaTags =
    response.status === 200
      ? response.body
      : { error: response.body || response.status };
  renderModels();
}

async function loadOpenAiModels() {
  const response = await api("proxy", "/v1/models", {
    expected: [200, 404, 500],
  });
  state.data.openaiModels =
    response.status === 200
      ? response.body
      : { error: response.body || response.status };
  renderModels();
}

function renderAll() {
  renderRole();
  renderOverview();
  renderModels();
  if (state.admin) {
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

  $("#metric-workers").textContent = state.admin ? workerCount : "-";
  $("#metric-models").textContent = modelIds.size || "-";
  $("#metric-queued").textContent = state.admin ? queued : "-";
  $("#overview-workers")
    .closest(".panel")
    .classList.toggle("hidden", !state.admin);
  $("#overview-queues")
    .closest(".panel")
    .classList.toggle("hidden", !state.admin);
  $("#overview-backends")
    .closest(".panel")
    .classList.toggle("hidden", !state.admin);
  renderOverviewWorkers();
  renderOverviewQueues(queued);
  renderOverviewBackends();
  renderOverviewModels(ollamaModels, openaiModels);
}

function renderOverviewWorkers() {
  const root = $("#overview-workers");
  if (!state.admin) {
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
  if (!state.admin) {
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
  if (!state.admin) {
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
  if (!state.admin) return;
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
  $("#keys-table").innerHTML = table(
    [
      "ID",
      "Name",
      "Token",
      "Role",
      "Capture",
      "Whitelist",
      "Blacklist",
      "Actions",
    ],
    keys.map((key) => [
      key.id,
      `<input data-key-name="${key.id}" value="${escapeAttr(key.name)}" />`,
      `<button class="token-copy" data-action="copy-token" data-token="${escapeAttr(key.token)}" title="Copy token">${escapeHtml(maskToken(key.token))}</button>`,
      escapeHtml(key.role),
      `<label class="check"><input data-key-capture="${key.id}" type="checkbox" ${key.capture ? "checked" : ""} /> capture</label>`,
      tags(key.whitelist_models),
      tags(key.blacklist_models),
      `<button class="small" data-action="save-key" data-id="${key.id}">Save</button>
       <button class="small" data-action="delete-key" data-id="${key.id}">Delete</button>`,
    ]),
  );
}

function renderWorkers() {
  if (!state.admin) return;
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
  if (!state.admin) return;
  $("#queue-json").textContent = pretty(state.data.queue || {});
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
  if (!state.admin) return;
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
  $("#usage-keys").innerHTML = sorted.length
    ? usageTable(
        "keys",
        [
          { label: "Client", field: "label", type: "text" },
          { label: "Req", field: "requests" },
          { label: "Err", field: "errors" },
          { label: "Input", field: "prompt_tokens" },
          { label: "Output", field: "completion_tokens" },
          { label: "Total", field: "total_tokens" },
          { label: "Time", field: "time" },
          { label: "Out tok/s", field: "out_tps" },
        ],
        sorted.map((row) =>
          usageRowCells(row, row.key_name || `key ${row.key_id ?? "-"}`),
        ),
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
  if (!name) return setStatus("Key name is required.", true);
  const payload = {
    name,
    role: $("#new-key-role").value,
    capture: $("#new-key-capture").checked,
    whitelist_models: splitCsv($("#new-key-whitelist").value),
    blacklist_models: splitCsv($("#new-key-blacklist").value),
  };
  const response = await api("management", "/key", {
    method: "POST",
    body: payload,
    expected: [201, 400, 409, 500],
  });
  if (response.status !== 201)
    return setStatus(`Create failed: ${response.status}`, true);
  $("#created-token").innerHTML =
    `Created token: <code>${escapeHtml(response.body.token)}</code>`;
  $("#create-key-form").reset();
  $("#new-key-capture").checked = true;
  await loadKeys();
}

async function copyToken(token) {
  try {
    await navigator.clipboard.writeText(token);
    setStatus("Token copied.");
  } catch {
    fallbackCopy(token);
    setStatus("Token copied.");
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
  const response = await api("management", "/key", {
    method: "PATCH",
    body: { id, name, capture },
    expected: [200, 400, 404, 409, 500],
  });
  if (response.status !== 200)
    return setStatus(`Save failed: ${response.status}`, true);
  setStatus("Key updated.");
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
    return setStatus(`Delete failed: ${response.status}`, true);
  setStatus("Key deleted.");
  await loadKeys();
}

async function sendWorkerCommand(event) {
  event.preventDefault();
  const worker = $("#command-worker").value.trim();
  const command = $("#command-name").value;
  if (!worker) return setStatus("Worker name is required.", true);
  const response = await api("management", "/worker/command", {
    method: "POST",
    body: { worker, command },
    expected: [202, 400, 500],
  });
  setStatus(
    response.status === 202
      ? "Worker command queued."
      : `Command failed: ${response.status}`,
    response.status !== 202,
  );
}

function selectModel(source, name, defaultMode) {
  state.selectedModel = {
    source,
    name,
    mode: defaultMode || "chat",
  };
  state.lastRequest = null;
  $("#model-console-mode").disabled = false;
  $("#model-run-button").disabled = false;
  renderModelConsole();
  renderRequestInfo();
  setPromptOutput("Response will appear here.", true);
}

function renderModelConsole() {
  const selected = state.selectedModel;
  if (!selected) {
    $("#model-console-title").textContent = "Select a model";
    $("#model-console-subtitle").textContent =
      "Choose a model from either list.";
    $("#model-console-mode").disabled = true;
    $("#model-run-button").disabled = true;
    renderRequestInfo();
    return;
  }
  $("#model-console-title").textContent = selected.name;
  $("#model-console-subtitle").textContent =
    selected.source === "ollama"
      ? "Ollama native endpoints"
      : "OpenAI-compatible endpoints";
  $("#model-console-mode").value = selected.mode;
}

function beginRequest(selected, path, method, body, input) {
  const bodyText = JSON.stringify(body);
  state.lastRequest = {
    id: Date.now(),
    state: "running",
    source: selected.source,
    mode: selected.mode,
    model: selected.name,
    method,
    path,
    status: null,
    startedAt: new Date(),
    finishedAt: null,
    durationMs: null,
    ttftMs: null,
    promptChars: input.length,
    requestBytes: byteLength(bodyText),
    requestBody: body,
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

async function runPrompt(event) {
  event.preventDefault();
  const selected = state.selectedModel;
  const input = $("#prompt-text").value;
  if (!selected) return setStatus("Select a model first.", true);
  setPromptOutput("Running...", true);
  setStatus(`Running ${selected.mode} on ${selected.name}...`);

  if (selected.mode === "embedding") {
    await runEmbedding(selected, input);
    setStatus("Embedding finished.");
    return;
  }

  if (selected.source === "openai") {
    const body = {
      model: selected.name,
      messages: [{ role: "user", content: input }],
      stream: true,
    };
    beginRequest(selected, "/v1/chat/completions", "POST", body, input);
    await streamRequest(
      "/v1/chat/completions",
      body,
      parseOpenAiStream,
    );
  } else {
    const body = {
      model: selected.name,
      messages: [{ role: "user", content: input }],
      stream: true,
    };
    beginRequest(selected, "/api/chat", "POST", body, input);
    await streamRequest(
      "/api/chat",
      body,
      parseOllamaChatStream,
    );
  }
  finishRequest("done");
  setStatus("Prompt finished.");
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
}

async function streamRequest(path, body, parser) {
  try {
    const response = await fetch(`/api/proxy${path}`, {
      method: "POST",
      headers: authHeaders(),
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
  const response = await fetch(`/api/${kind}${path}`, {
    method,
    headers: authHeaders(options.body),
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

async function requestJson(path) {
  const response = await fetch(path);
  if (!response.ok) throw new Error(`${path} returned ${response.status}`);
  return response.json();
}

function authHeaders(hasBody = true) {
  const headers = {};
  if (hasBody) headers["content-type"] = "application/json";
  if (state.key) {
    headers.authorization = `Bearer ${state.key}`;
    headers["api-key"] = state.key;
  }
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
      endpoints: group.endpoints.filter(
        (endpoint) => state.admin || endpoint[2] === "client",
      ),
    }))
    .filter((group) => group.endpoints.length > 0)
    .map(
      (group) => `
        <section class="api-card">
          <h3>${escapeHtml(group.title)}</h3>
          <div class="endpoint-list">
            ${group.endpoints
              .map(
                ([method, path, scope]) => `
                  <div class="endpoint">
                    <span class="method ${method.toLowerCase()}">${method}</span>
                    <div>
                      <div class="path">${escapeHtml(path)}</div>
                      <div class="scope">${escapeHtml(scope)}</div>
                    </div>
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
