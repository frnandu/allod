# HTML for the kiosk's local setup pages (busybox httpd CGI on 127.0.0.1:8030; sourced,
# POSIX sh). Plain page loads and forms only: LibreWolf holds a page's own requests until
# the user interacts with it, so the setup flow uses no JavaScript.

# Escapes text for HTML.
h() { printf '%s' "$1" | sed -e 's/&/\&amp;/g' -e 's/</\&lt;/g' -e 's/>/\&gt;/g' -e 's/"/\&quot;/g'; }

# Response headers, then the page head. $1: page title, $2: optional meta refresh seconds.
page_start() {
  printf 'Content-Type: text/html; charset=utf-8\r\nCache-Control: no-store\r\n\r\n'
  cat <<EOF
<!doctype html>
<html lang="en">
<head>
<meta charset="utf-8">
${2:+<meta http-equiv="refresh" content="$2">}
<title>$(h "$1")</title>
<style>
  :root { --bg: #0b0e11; --surface: #12171c; --line: #222b33; --fg: #e6ebef; --muted: #8a96a1;
          --accent: #2dd4bf; --danger: #ef4444; --btc: #f7931a; }
  * { box-sizing: border-box; }
  html, body { margin: 0; min-height: 100%; background: var(--bg); color: var(--fg);
    font: 16px/1.45 "DejaVu Sans", sans-serif; }
  main { max-width: 44rem; margin: 0 auto; padding: 48px 24px; }
  .center { min-height: 100vh; display: flex; flex-direction: column; align-items: center;
    justify-content: center; gap: 28px; text-align: center; padding: 24px; }
  .logo { width: 360px; max-width: 70vw; height: auto; }
  .small .logo { width: 200px; }
  .dot { width: 10px; height: 10px; border-radius: 50%; background: var(--btc);
    animation: pulse 1.6s ease-in-out infinite; }
  @keyframes pulse { 50% { opacity: .3; } }
  .muted { color: var(--muted); }
  h1 { font-size: 1.6rem; margin: 24px 0 6px; }
  p { margin: 0 0 16px; }
  .opts { display: grid; gap: 10px; margin: 20px 0; }
  .opt { display: flex; gap: 14px; align-items: flex-start; padding: 14px 16px; border-radius: 12px;
    border: 1px solid var(--line); background: var(--surface); cursor: pointer; }
  .opt:has(input:checked) { border-color: var(--accent); }
  .opt input { margin-top: 4px; accent-color: var(--accent); width: 18px; height: 18px; }
  .opt .t { font-weight: 600; }
  .opt .d { color: var(--muted); font-size: .92rem; }
  .erase { color: var(--danger); font-weight: 600; }
  .confirm { display: none; }
  form:has(input[data-erase]:checked) .confirm { display: block; }
  label.f { display: block; margin: 14px 0 6px; font-size: .95rem; }
  input.text { width: 100%; padding: 12px; border-radius: 10px; border: 1px solid var(--line);
    background: var(--surface); color: var(--fg); font: inherit; }
  .row { display: flex; gap: 12px; margin-top: 20px; flex-wrap: wrap; align-items: center; }
  button, a.btn { padding: 12px 22px; border-radius: 999px; font: inherit; font-weight: 600;
    cursor: pointer; text-decoration: none; }
  button { border: 0; background: var(--accent); color: #04201d; }
  form:has(input[data-erase]:checked) button { background: var(--danger); color: #fff; }
  a.btn { border: 1px solid var(--line); color: var(--fg); }
  button:focus-visible, a.btn:focus-visible, .opt input:focus-visible, input.text:focus-visible {
    outline: 2px solid var(--accent); outline-offset: 2px; }
  .error { color: var(--danger); margin-top: 12px; }
  .hint { color: var(--muted); font-size: .85rem; margin-top: 28px; }
</style>
</head>
<body>
EOF
}

logo() {
  cat <<'EOF'
<svg class="logo" viewBox="0 0 120 32" aria-label="Allod">
  <rect width="32" height="32" rx="9" fill="#2dd4bf"/>
  <path d="M8 24 16 9.5 24 24M11.6 18.5h8.8" fill="none" stroke="#04201d" stroke-width="3" stroke-linecap="round" stroke-linejoin="round"/>
  <circle cx="16" cy="9.5" r="2.6" fill="#f7931a"/>
  <text x="42" y="24" font-family="DejaVu Sans" font-weight="bold" font-size="22" fill="#e6ebef">Allod</text>
</svg>
EOF
}

page_end() { printf '</body>\n</html>\n'; }

# Value of field $1 in an application/x-www-form-urlencoded string $2, decoded (awk: dash's
# printf has no \x escapes).
form_value() {
  printf '%s' "$2" | tr '&' '\n' | sed -n "s/^$1=//p" | head -1 | awk '
    BEGIN { for (i = 1; i < 256; i++) hex[sprintf("%02X", i)] = sprintf("%c", i) }
    { s = $0; gsub(/\+/, " ", s); out = ""
      while (match(s, /%[0-9A-Fa-f][0-9A-Fa-f]/)) {
        out = out substr(s, 1, RSTART - 1) hex[toupper(substr(s, RSTART + 1, 2))]
        s = substr(s, RSTART + 3)
      }
      printf "%s", out s }'
}

# Redirect (busybox httpd only passes a status through when the CGI writes the status line).
redirect() { # status-line location
  printf 'HTTP/1.1 %s\r\nLocation: %s\r\nCache-Control: no-store\r\nContent-Length: 0\r\n\r\n' "$1" "$2"
}
