<?php

declare(strict_types=1);

const PROXY_NAME = 'mcp-filesystem-php-proxy';
const PROXY_VERSION = '1.1.0';

header_remove('X-Powered-By');

function jsonResponse(int $status, array $data): never
{
    http_response_code($status);
    header('Content-Type: application/json; charset=utf-8');
    header('Cache-Control: no-store');
    echo json_encode($data, JSON_UNESCAPED_SLASHES | JSON_UNESCAPED_UNICODE | JSON_PRETTY_PRINT);
    exit;
}

function envString(string $name, string $default): string
{
    $value = getenv($name);
    return is_string($value) && $value !== '' ? $value : $default;
}

function envInt(string $name, int $default): int
{
    $value = getenv($name);
    return is_string($value) && preg_match('/^\d+$/', $value) === 1 ? (int) $value : $default;
}

function envBool(string $name, bool $default): bool
{
    $value = getenv($name);
    return is_string($value) && $value !== ''
        ? (filter_var($value, FILTER_VALIDATE_BOOL, FILTER_NULL_ON_FAILURE) ?? $default)
        : $default;
}

function normalizeBasePath(string $path): string
{
    $path = '/' . trim($path, '/');
    return $path === '/' ? '' : $path;
}

function requestHeader(string $name): ?string
{
    $key = 'HTTP_' . strtoupper(str_replace('-', '_', $name));
    foreach ([$_SERVER[$key] ?? null, $_SERVER['REDIRECT_' . $key] ?? null] as $value) {
        if (is_string($value) && $value !== '') {
            return $value;
        }
    }

    if (function_exists('getallheaders')) {
        foreach ((array) getallheaders() as $header => $value) {
            if (is_string($header) && strcasecmp($header, $name) === 0 && is_string($value)) {
                return $value;
            }
        }
    }

    return null;
}

function requestScheme(): string
{
    $forwarded = $_SERVER['HTTP_X_FORWARDED_PROTO'] ?? '';
    if (is_string($forwarded) && $forwarded !== '') {
        return strtolower(trim(explode(',', $forwarded)[0]));
    }
    return (!empty($_SERVER['HTTPS']) && $_SERVER['HTTPS'] !== 'off') ? 'https' : 'http';
}

function publicUrl(string $path): string
{
    return requestScheme() . '://' . ($_SERVER['HTTP_HOST'] ?? 'localhost') . $path;
}

function loadConfig(): array
{
    $allowed = array_values(array_filter(array_map(
        'trim',
        explode(',', envString('MCP_PROXY_ALLOWED_PATHS', '/mcp,/rpc,/health,/version,/info,/tools'))
    )));

    $config = [
        'backend_url' => envString('MCP_BACKEND_URL', 'http://services-b24.alwaysdata.net:8345'),
        'base_path' => envString('MCP_PROXY_BASE_PATH', '/mcpfs'),
        'connect_timeout' => envInt('MCP_PROXY_CONNECT_TIMEOUT', 5),
        'request_timeout' => envInt('MCP_PROXY_REQUEST_TIMEOUT', 60),
        'max_request_bytes' => envInt('MCP_PROXY_MAX_REQUEST_BYTES', 16 * 1024 * 1024),
        'allowed_paths' => $allowed,
        'forward_original_host' => envBool('MCP_PROXY_FORWARD_ORIGINAL_HOST', false),
    ];

    if (is_file(__DIR__ . '/config.php')) {
        $local = require __DIR__ . '/config.php';
        if (!is_array($local)) {
            throw new RuntimeException('config.php must return an array');
        }
        $config = array_replace($config, $local);
    }

    $config['backend_url'] = rtrim((string) $config['backend_url'], '/');
    $config['base_path'] = normalizeBasePath((string) $config['base_path']);
    if (!in_array(parse_url($config['backend_url'], PHP_URL_SCHEME), ['http', 'https'], true)) {
        throw new RuntimeException('backend_url must use http or https');
    }

    return $config;
}

function prepareResponseStreaming(): void
{
    @ini_set('output_buffering', '0');
    @ini_set('zlib.output_compression', '0');
    @ini_set('implicit_flush', '1');
    if (function_exists('apache_setenv')) {
        @apache_setenv('no-gzip', '1');
    }
    while (ob_get_level() > 0) {
        @ob_end_flush();
    }
    ob_implicit_flush(true);
}

function forwardRequestHeaders(array $config, int $bodyLength): array
{
    $blocked = [
        'connection', 'content-length', 'expect', 'host', 'keep-alive',
        'proxy-authenticate', 'proxy-authorization', 'te', 'trailer',
        'transfer-encoding', 'upgrade', 'x-forwarded-for',
        'x-forwarded-host', 'x-forwarded-port', 'x-forwarded-proto',
    ];
    $incoming = function_exists('getallheaders') ? (array) getallheaders() : [];
    if (($authorization = requestHeader('Authorization')) !== null) {
        $incoming['Authorization'] = $authorization;
    }

    $headers = [];
    foreach ($incoming as $name => $value) {
        if (is_string($name) && (is_string($value) || is_numeric($value))
            && !in_array(strtolower($name), $blocked, true)) {
            $headers[] = $name . ': ' . trim((string) $value);
        }
    }

    if ($bodyLength > 0) {
        $headers[] = 'Content-Length: ' . $bodyLength;
    }
    if (!empty($_SERVER['REMOTE_ADDR'])) {
        $headers[] = 'X-Forwarded-For: ' . $_SERVER['REMOTE_ADDR'];
    }
    $headers[] = 'X-Forwarded-Proto: ' . requestScheme();
    $headers[] = 'X-Forwarded-Host: ' . ($_SERVER['HTTP_HOST'] ?? '');
    $headers[] = 'X-MCP-Proxy: ' . PROXY_NAME . '/' . PROXY_VERSION;

    if (!empty($config['forward_original_host']) && !empty($_SERVER['HTTP_HOST'])) {
        $headers[] = 'Host: ' . $_SERVER['HTTP_HOST'];
    }

    return $headers;
}

function forwardResponseHeader(string $name): bool
{
    return !in_array(strtolower($name), [
        'connection', 'content-length', 'keep-alive', 'proxy-authenticate',
        'proxy-authorization', 'te', 'trailer', 'transfer-encoding', 'upgrade',
    ], true);
}

function backendHealth(string $backendUrl, int $timeout): never
{
    $curl = curl_init($backendUrl . '/health');
    curl_setopt_array($curl, [
        CURLOPT_RETURNTRANSFER => true,
        CURLOPT_CONNECTTIMEOUT => max(1, $timeout),
        CURLOPT_TIMEOUT => max(5, $timeout),
        CURLOPT_HTTPHEADER => ['Accept: application/json'],
    ]);
    $body = curl_exec($curl);
    $status = (int) curl_getinfo($curl, CURLINFO_RESPONSE_CODE);
    $error = curl_error($curl);
    curl_close($curl);

    if ($body === false || $status < 200 || $status >= 400) {
        jsonResponse(503, ['status' => 'down', 'backendHttpCode' => $status, 'message' => $error]);
    }

    $decoded = json_decode($body, true);
    jsonResponse(200, ['status' => 'up', 'backendHttpCode' => $status, 'backend' => $decoded ?? $body]);
}

try {
    $config = loadConfig();
} catch (Throwable $error) {
    jsonResponse(500, ['status' => 'error', 'error' => 'proxy_configuration_error', 'message' => $error->getMessage()]);
}

$requestUri = $_SERVER['REQUEST_URI'] ?? '/';
$requestPath = parse_url($requestUri, PHP_URL_PATH) ?: '/';
$basePath = $config['base_path'];

if ($basePath !== '' && $requestPath !== $basePath && !str_starts_with($requestPath, $basePath . '/')) {
    jsonResponse(404, ['status' => 'error', 'error' => 'not_found']);
}

$relativePath = $basePath === '' ? $requestPath : substr($requestPath, strlen($basePath));
$relativePath = $relativePath === '' ? '/' : $relativePath;
$method = strtoupper($_SERVER['REQUEST_METHOD'] ?? 'GET');

if ($relativePath === '/' && $method === 'GET') {
    jsonResponse(200, [
        'status' => 'ok',
        'service' => PROXY_NAME,
        'version' => PROXY_VERSION,
        'mcpEndpoint' => publicUrl($basePath . '/mcp'),
        'healthEndpoint' => publicUrl($basePath . '/health'),
        'transport' => 'streamable-http',
        'supportedMethods' => ['POST', 'DELETE'],
    ]);
}

if ($relativePath === '/health') {
    jsonResponse(200, ['status' => 'ok', 'service' => PROXY_NAME, 'version' => PROXY_VERSION, 'time' => gmdate(DATE_ATOM)]);
}

if ($relativePath === '/mcp' && $method === 'GET') {
    header('Allow: POST, DELETE');
    jsonResponse(405, [
        'jsonrpc' => '2.0',
        'error' => [
            'code' => -32000,
            'message' => 'GET /mcp is not enabled by this PHP proxy; use MCP Streamable HTTP over POST.',
        ],
        'id' => null,
    ]);
}

if (!function_exists('curl_init')) {
    jsonResponse(500, ['status' => 'error', 'error' => 'php_curl_extension_required']);
}

if ($relativePath === '/health/backend') {
    backendHealth($config['backend_url'], (int) $config['connect_timeout']);
}

$allowed = array_map(static fn ($path) => '/' . ltrim((string) $path, '/'), (array) $config['allowed_paths']);
if ($allowed !== [] && !in_array($relativePath, $allowed, true)) {
    jsonResponse(404, ['status' => 'error', 'error' => 'path_not_proxied', 'path' => $relativePath]);
}

$query = parse_url($requestUri, PHP_URL_QUERY);
$backendUrl = $config['backend_url'] . $relativePath . (is_string($query) && $query !== '' ? '?' . $query : '');
$body = file_get_contents('php://input');
if ($body === false) {
    jsonResponse(400, ['status' => 'error', 'error' => 'request_body_read_failed']);
}
if (strlen($body) > (int) $config['max_request_bytes']) {
    jsonResponse(413, ['status' => 'error', 'error' => 'request_too_large', 'maxBytes' => $config['max_request_bytes']]);
}

prepareResponseStreaming();

$started = false;
$status = 502;
$curl = curl_init($backendUrl);
curl_setopt_array($curl, [
    CURLOPT_CUSTOMREQUEST => $method,
    CURLOPT_RETURNTRANSFER => false,
    CURLOPT_FOLLOWLOCATION => false,
    CURLOPT_HTTP_VERSION => CURL_HTTP_VERSION_1_1,
    CURLOPT_CONNECTTIMEOUT => max(1, (int) $config['connect_timeout']),
    CURLOPT_TIMEOUT => max(0, (int) $config['request_timeout']),
    CURLOPT_HTTPHEADER => forwardRequestHeaders($config, strlen($body)),
    CURLOPT_HEADERFUNCTION => static function ($handle, string $line) use (&$started, &$status): int {
        $trimmed = trim($line);
        if (preg_match('#^HTTP/\S+\s+(\d{3})#i', $trimmed, $matches) === 1) {
            $status = (int) $matches[1];
            http_response_code($status);
            $started = true;
        } elseif (($colon = strpos($line, ':')) !== false) {
            $name = trim(substr($line, 0, $colon));
            if (forwardResponseHeader($name)) {
                header($name . ': ' . trim(substr($line, $colon + 1)), false);
            }
        }
        return strlen($line);
    },
    CURLOPT_WRITEFUNCTION => static function ($handle, string $chunk) use (&$started, &$status): int {
        if (!$started) {
            http_response_code($status);
            $started = true;
        }
        echo $chunk;
        @ob_flush();
        flush();
        return connection_aborted() ? 0 : strlen($chunk);
    },
]);

if ($body !== '' || in_array($method, ['POST', 'PUT', 'PATCH'], true)) {
    curl_setopt($curl, CURLOPT_POSTFIELDS, $body);
}

$ok = curl_exec($curl);
$error = curl_error($curl);
$errno = curl_errno($curl);
$backendStatus = (int) curl_getinfo($curl, CURLINFO_RESPONSE_CODE);
curl_close($curl);

if ($ok === false && !$started && !headers_sent()) {
    jsonResponse(502, ['status' => 'error', 'error' => 'backend_unavailable', 'curlCode' => $errno, 'message' => $error]);
}
if (!$started && !headers_sent()) {
    http_response_code($backendStatus > 0 ? $backendStatus : 502);
}
