# Ponytail cuts — hex-cli only

De-over-engineering pass scoped to `crates/hex-cli/`. Every item is a cut or a
shrink: same behavior, less code. Do NOT touch any other crate. Line numbers are
against the branch base; re-locate by symbol if they drifted. After each item:
`cargo build --bin hex` and `cargo test -p hex-cli` must pass.

- [ ] ui.rs: `Ui::stdout()` and `Ui::stderr()` (ui.rs:43-68) are byte-identical
  except which stream is probed for TTY-ness. Collapse into one private
  `fn for_stream(color, json, is_tty) -> Ui` with two 1-line public callers.
- [ ] ui.rs: `pub fn cells` (ui.rs:385-388) has no caller outside its module —
  make it private. Also delete the orphaned doc-comment sentence at ui.rs:577
  that documents a test (`no_glyph_is_an_emoji_presentation_codepoint`) which no
  longer exists.
- [ ] main.rs: `grey()` (main.rs:1939) is a second, hand-rolled color policy —
  raw `\x1b[90m` plus its own `NO_COLOR` read — threaded as a `tty: bool`
  through ~6 signatures and ~12 call sites (main.rs:1076, 1680, 1738, 1839,
  1882, 1905), and it ignores `--color`. Replace each use with the existing
  `Ui` dim-paint path (`ui.paint(...)` with the dim style, as the rest of the
  file does) and delete `grey()` and the threaded `tty` parameters.
- [ ] graph_view.rs: a `g: &Glyphs` parameter is threaded beside `ui: Ui`
  through ~10 signatures (graph_view.rs:27,51,95,124,220,236,255,325,359,411
  and main.rs:789), but it is exactly `ui.glyphs()` and `Ui` is `Copy`. Drop
  the parameter and call `ui.glyphs()` where needed.
- [ ] graph_view.rs: `badge_painted` (graph_view.rs:220-248) re-matches
  `spec.kind()` and re-tests the terminal disposition that `badge` just
  matched. Refactor to one fn returning `(glyph, style)` — the shape
  `dash::mark_cell` already uses.
- [ ] dash.rs: `hue_to_ratatui` (dash.rs:221-240) maps all 16 `AnsiColor`
  variants, but its only caller feeds it `Mark::hue()`, which only ever
  returns Green/Red/Yellow/Cyan. Keep those 4 arms plus a `_ => Color::Reset`
  fallback.
- [ ] main.rs: `RunMode` (main.rs:359-362) is a 2-field struct with one
  construction site and one read (main.rs:832-841), existing only to shorten
  an arg list that is already `#[allow(clippy::too_many_arguments)]`. Inline
  the two fields as parameters. Also: `age()` (main.rs:1186) re-derives
  `now_ms().saturating_sub(at_ms)` which is exactly `elapsed_ms` (main.rs
  a few functions above) — call it instead.
- [ ] main.rs: `--no-preview` is declared `global = true` (main.rs:78-79), so
  13 verbs accept and silently ignore it. Move it onto the `run` and `resume`
  subcommands only, and drop the global threading (main.rs:280,317,320). A
  flag a command accepts and ignores is worse than one it rejects. Keep
  `hex run --help` showing it.
- [ ] main.rs tests: delete the 6 trivial clap-behaviour unit tests (they
  assert clap-derive config, not hex logic; written against a hand-rolled
  parser deleted long ago): main.rs:2168, 2187, 2193, 2239, 2247, 2252 —
  the "-p needs a value", "-p+-f conflict", "ls alias", "bare hex has no
  command", "--json parses first", "-p rejected on validate" tests. The
  integration survivor is cli.rs `clap_rejects_bad_invocations_with_exit_2`.
  Then merge the 4 remaining prompt-source tests (main.rs:2147,2174,2198,2207
  — positional / `--prompt` / none / `-f` file, identical
  parse→resolve_prompt→assert shape) into one table-driven test with the case
  name in the assert message.
- [ ] tests/cli.rs: merge `run_with_prompt_flag_succeeds` and
  `run_with_prompt_file_succeeds` (cli.rs:285-304, same assertions, only the
  prompt source varies) into one table-driven test. Fold the one unique assert
  of `help_and_version_exit_0` (cli.rs:201-215) — the `dash -h` json-exception
  — into `dash_refuses_json_because_it_has_no_machine_mode` (cli.rs:189) and
  delete the rest of it (its `--help`/`--version` asserts are already made at
  cli.rs:162-167).
- [ ] preview.rs: delete the file-shrank restart branch (preview.rs:514-518)
  and the test pinning it (preview.rs:847-858) — the same module documents the
  case as impossible ("logs are created once at spawn and only appended",
  preview.rs:470-473). Then trim the `fmt_mmss` unit test (preview.rs:618-624)
  to only the `3600 → 60:00` rollover case; the 0:00/0:47/1:30 cases are
  re-made through the real caller by
  `status_line_shows_node_worker_attempt_timer_and_countdown`.
