# Find operations that do not belong in Rust unit tests. This is a deliberately
# small lexer: it removes comments and string/character literals before
# tracking #[cfg(test)] item braces, so examples in docs and test data do not
# become policy findings.

function blank(character) {
  return character == "\n" ? character : " "
}

function mask(line,    out, i, c, nextc, j, hashes, terminator, line_length, closing) {
  # Most Rust lines contain no token that can start a comment or literal. The
  # fast path matters on large inline test modules and keeps this source-only
  # gate comfortably below a second on the CI class of host.
  if (lexer == "code" && line !~ /[\/"\047]/) return line

  out = ""
  line_length = length(line)
  i = 1
  while (i <= line_length) {
    c = substr(line, i, 1)
    nextc = substr(line, i + 1, 1)

    if (lexer == "line-comment") lexer = "code"

    if (lexer == "block-comment") {
      if (c == "/" && nextc == "*") {
        out = out "  "; i += 2; block_depth++; continue
      }
      if (c == "*" && nextc == "/") {
        out = out "  "; i += 2; block_depth--
        if (block_depth == 0) lexer = "code"
        continue
      }
      out = out " "; i++; continue
    }

    if (lexer == "string") {
      if (c == "\\" && i < line_length) { out = out "  "; i += 2; continue }
      out = out " "; i++
      if (c == "\"") lexer = "code"
      continue
    }

    if (lexer == "raw-string") {
      terminator = "\""
      for (j = 0; j < raw_hashes; j++) terminator = terminator "#"
      if (substr(line, i, length(terminator)) == terminator) {
        for (j = 0; j < length(terminator); j++) out = out " "
        i += length(terminator); lexer = "code"
      } else { out = out " "; i++ }
      continue
    }

    if (c == "/" && nextc == "/") {
      while (i <= line_length) { out = out " "; i++ }
      lexer = "line-comment"
      continue
    }
    if (c == "/" && nextc == "*") {
      out = out "  "; i += 2; lexer = "block-comment"; block_depth = 1; continue
    }
    if (c == "\"") { out = out " "; i++; lexer = "string"; continue }
    if (c == "r") {
      j = i + 1; hashes = 0
      while (substr(line, j, 1) == "#") { hashes++; j++ }
      if (substr(line, j, 1) == "\"") {
        while (i <= j) { out = out " "; i++ }
        raw_hashes = hashes; lexer = "raw-string"; continue
      }
    }
    if (c == "\047") {
      closing = 0
      for (j = i + 1; j <= line_length && j <= i + 4; j++) {
        if (substr(line, j, 1) == "\047") { closing = j; break }
      }
      if (closing > 0) {
        while (i <= closing) { out = out " "; i++ }
        continue
      }
    }
    out = out c
    i++
  }
  return out
}

function occurrences(text, regex,    count) {
  count = 0
  while (match(text, regex)) {
    count++
    text = substr(text, RSTART + RLENGTH)
  }
  return count
}

function add(kind, amount, line_number) {
  if (amount < 1) return
  counts[kind] += amount
  if (lines[kind] != "") lines[kind] = lines[kind] ","
  lines[kind] = lines[kind] line_number
}

function flush(kind) {
  for (kind in counts) {
    printf "%s\t%s\t%d\t%s\n", current_file, kind, counts[kind], lines[kind]
  }
  for (kind in counts) delete counts[kind]
  for (kind in lines) delete lines[kind]
}

FNR == 1 {
  if (NR > 1) flush()
  current_file = FILENAME
  lexer = "code"
  block_depth = 0
  raw_hashes = 0
  test_depth = 0
  pending_cfg_test = 0
}

{
  # Unit-test modules are conventionally at the end of each source file. Do
  # not lex production code byte-by-byte while looking for them; the anchored
  # attribute is safe to identify before comment/string masking.
  if (test_depth == 0 && !pending_cfg_test) {
    if ($0 ~ /^[[:space:]]*#[[:space:]]*\[[[:space:]]*cfg[[:space:]]*\([[:space:]]*test[[:space:]]*\)[[:space:]]*\]/) {
      pending_cfg_test = 1
    } else {
      next
    }
  }

  # Once inside a test item, most source lines cannot affect either brace
  # depth or a hazard finding. Avoid the character lexer for those lines.
  # An odd number of unescaped quotes keeps the slow path for the uncommon
  # multi-line ordinary string; raw strings and block comments do likewise.
  if (test_depth > 0 && lexer == "code" &&
      $0 !~ /[{}]|sleep|Command|set_var|remove_var|\/\*|\*\/|r#*"/) {
    quote_probe = $0
    gsub(/\\\\/, "", quote_probe)
    gsub(/\\"/, "", quote_probe)
    quote_count = gsub(/"/, "\"", quote_probe)
    if (quote_count % 2 == 0) next
  }

  code = mask($0)

  if (code ~ /#[[:space:]]*\[[[:space:]]*cfg[[:space:]]*\([[:space:]]*test[[:space:]]*\)[[:space:]]*\]/) {
    pending_cfg_test = 1
  }

  entering = 0
  if (pending_cfg_test && code ~ /(mod[[:space:]]+[A-Za-z_][A-Za-z0-9_]*|((async[[:space:]]+)?fn[[:space:]]+[A-Za-z_][A-Za-z0-9_]*))[^{;]*\{/) {
    entering = 1
    pending_cfg_test = 0
  } else if (pending_cfg_test && code !~ /^[[:space:]]*(#\[|$)/) {
    pending_cfg_test = 0
  }

  if (test_depth > 0 || entering) {
    add("sleep", occurrences(code, "(^|[^[:alnum:]_])((std[[:space:]]*::[[:space:]]*thread|tokio[[:space:]]*::[[:space:]]*time|thread|time)[[:space:]]*::[[:space:]]*)sleep[[:space:]]*\\("), FNR)
    add("process", occurrences(code, "(^|[^[:alnum:]_])((std|tokio)[[:space:]]*::[[:space:]]*process[[:space:]]*::[[:space:]]*)?Command[[:space:]]*::[[:space:]]*new[[:space:]]*\\("), FNR)
    add("global-env", occurrences(code, "(^|[^[:alnum:]_])(std[[:space:]]*::[[:space:]]*)?env[[:space:]]*::[[:space:]]*(set_var|remove_var)[[:space:]]*\\("), FNR)
  }

  opens = gsub(/\{/, "{", code)
  closes = gsub(/\}/, "}", code)
  if (entering) test_depth = opens - closes
  else if (test_depth > 0) test_depth += opens - closes
  if (test_depth < 1) test_depth = 0
}

END { flush() }
