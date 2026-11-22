# Reads ldk-server's Cargo.lock and prints the ldk-node and rust-lightning (`lightning`)
# packages it is built with, for Allod's node page:
#   {"ldk_node":{"version":"0.8.0+git","rev":"179b4b2…"},"lightning":{"version":…,"rev":…}}
# rev is the git commit of a git dependency, empty for one from crates.io; a package that
# is missing prints null.
function flush() {
  if ((name == "ldk-node" || name == "lightning") && !(name in out))
    out[name] = sprintf("{\"version\":\"%s\",\"rev\":\"%s\"}", version, rev)
}
BEGIN { FS = " = " }
/^\[\[package\]\]/ { flush(); name = version = rev = "" }
/^name = /    { gsub(/"/, "", $2); name = $2 }
/^version = / { gsub(/"/, "", $2); version = $2 }
/^source = /  { gsub(/"/, "", $2); n = split($2, a, "#"); rev = (n > 1 ? a[2] : "") }
END {
  flush()
  printf "{\"ldk_node\":%s,\"lightning\":%s}\n", \
    ("ldk-node" in out ? out["ldk-node"] : "null"), ("lightning" in out ? out["lightning"] : "null")
}
