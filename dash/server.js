const http = require("node:http");
const fs = require("node:fs");
const path = require("node:path");

const root = __dirname;
const publicTypes = new Map([
  [".html", "text/html; charset=utf-8"],
  [".css", "text/css; charset=utf-8"],
  [".js", "application/javascript; charset=utf-8"],
  [".json", "application/json; charset=utf-8"],
  [".svg", "image/svg+xml"],
]);

const env = loadEnv(path.join(root, ".env"));
const config = {
  proxyEndpoint: stripTrailingSlash(
    env.HIVECORE_PROXY_ENDPOINT || env.HIVECORE_ENDPOINT || "http://127.0.0.1:6666",
  ),
  managementEndpoint: stripTrailingSlash(
    env.HIVECORE_MANAGEMENT_ENDPOINT || "http://127.0.0.1:6668",
  ),
  key: env.KEY || "",
};

const host = env.DASH_HOST || "127.0.0.1";
const port = Number(env.DASH_PORT || 8088);

const server = http.createServer(async (request, response) => {
  try {
    const url = new URL(request.url, `http://${request.headers.host || "localhost"}`);

    if (url.pathname === "/config.json") {
      return json(response, 200, {
        proxyEndpoint: config.proxyEndpoint,
        managementEndpoint: config.managementEndpoint,
        key: config.key,
      });
    }

    if (url.pathname.startsWith("/api/management/")) {
      const targetPath = url.pathname.slice("/api/management".length) + url.search;
      return proxy(request, response, config.managementEndpoint, targetPath);
    }

    if (url.pathname.startsWith("/api/proxy/")) {
      const targetPath = url.pathname.slice("/api/proxy".length) + url.search;
      return proxy(request, response, config.proxyEndpoint, targetPath);
    }

    return serveStatic(url.pathname, response);
  } catch (error) {
    console.error(error);
    text(response, 500, "Internal dashboard server error");
  }
});

server.listen(port, host, () => {
  console.log(`HiveCore dashboard listening on http://${host}:${port}`);
  console.log(`Proxy endpoint: ${config.proxyEndpoint}`);
  console.log(`Management endpoint: ${config.managementEndpoint}`);
});

function loadEnv(file) {
  if (!fs.existsSync(file)) return {};
  const values = {};
  for (const line of fs.readFileSync(file, "utf8").split(/\r?\n/)) {
    const trimmed = line.trim();
    if (!trimmed || trimmed.startsWith("#")) continue;
    const index = trimmed.indexOf("=");
    if (index === -1) continue;
    const key = trimmed.slice(0, index).trim();
    let value = trimmed.slice(index + 1).trim();
    if (
      (value.startsWith('"') && value.endsWith('"')) ||
      (value.startsWith("'") && value.endsWith("'"))
    ) {
      value = value.slice(1, -1);
    }
    values[key] = value;
  }
  return values;
}

function stripTrailingSlash(value) {
  return value.replace(/\/+$/, "");
}

function serveStatic(urlPath, response) {
  const cleanPath = urlPath === "/" ? "/index.html" : decodeURIComponent(urlPath);
  const filePath = path.normalize(path.join(root, cleanPath));
  if (!filePath.startsWith(root)) {
    return text(response, 403, "Forbidden");
  }
  fs.readFile(filePath, (error, data) => {
    if (error) return text(response, 404, "Not found");
    const type = publicTypes.get(path.extname(filePath)) || "application/octet-stream";
    response.writeHead(200, { "content-type": type, "cache-control": "no-store" });
    response.end(data);
  });
}

function proxy(clientRequest, clientResponse, endpoint, targetPath) {
  const target = new URL(targetPath, endpoint);
  const headers = { ...clientRequest.headers };
  delete headers.host;
  delete headers.connection;
  const chunks = [];
  clientRequest.on("data", (chunk) => chunks.push(chunk));
  clientRequest.on("end", () => {
    const body = Buffer.concat(chunks);
    if (body.length > 0) {
      headers["content-length"] = String(body.length);
    } else {
      delete headers["content-length"];
    }

    const upstream = (target.protocol === "https:" ? require("node:https") : http).request(
      target,
      {
        method: clientRequest.method,
        headers,
      },
      (upstreamResponse) => {
        const responseHeaders = { ...upstreamResponse.headers };
        responseHeaders["access-control-allow-origin"] = "*";
        clientResponse.writeHead(upstreamResponse.statusCode || 502, responseHeaders);
        upstreamResponse.pipe(clientResponse);
      },
    );

    upstream.on("error", (error) => {
      json(clientResponse, 502, {
        error: "upstream_unreachable",
        message: error.message,
        target: target.toString(),
      });
    });

    upstream.end(body);
  });
}

function json(response, status, value) {
  response.writeHead(status, {
    "content-type": "application/json; charset=utf-8",
    "cache-control": "no-store",
  });
  response.end(JSON.stringify(value));
}

function text(response, status, value) {
  response.writeHead(status, {
    "content-type": "text/plain; charset=utf-8",
    "cache-control": "no-store",
  });
  response.end(value);
}
