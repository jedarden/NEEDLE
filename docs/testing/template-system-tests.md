# Template System Test Documentation

This document describes the comprehensive test suite for NEEDLE's template rendering system.

## Overview

The template system is used throughout NEEDLE to render bead commands, prompts, and various output strings with dynamic bead data. The test suite ensures robust handling of all edge cases, error conditions, and performance requirements.

## Test Files

### 1. `src/template.rs` - Unit Tests
**Location**: Inline tests in the template module
**Coverage**: Basic rendering functionality, placeholder extraction, context creation

Key test categories:
- Basic template rendering
- Multiple placeholders
- All placeholder types
- Missing/empty values
- Unicode and special characters
- Placeholder extraction
- Edge cases (whitespace preservation, duplicates, etc.)

### 2. `tests/template_rendering_tests.rs` - Integration Tests
**Location**: Integration test suite
**Coverage**: Full CLI backend operation rendering with real bead data

Key test categories:
- Rendering bead-rs CLI operations
- Rendering bead-forge CLI operations
- All placeholder types ({id}, {actor}, {limit}, {title}, {body}, etc.)
- Empty values and error cases
- Unicode support
- Operations without placeholders

### 3. `tests/template_comprehensive_tests.rs` - Comprehensive Suite
**Location**: Comprehensive integration and edge case tests (NEW)
**Coverage**: Edge cases, error paths, performance benchmarks

## Test Categories in Comprehensive Suite

### Edge Case Tests

Tests for boundary conditions and unusual but valid inputs:

1. **Empty Templates**
   - `test_render_empty_template`: Rendering with empty string input
   - `test_render_template_with_only_placeholders`: Template containing only placeholders
   - `test_render_template_with_only_whitespace`: Whitespace-only templates

2. **Special Characters and Formatting**
   - `test_render_with_newlines_in_template`: Multi-line templates
   - `test_render_with_special_characters_in_values`: Values with `$`, `%`, `<`, `>`, quotes, backslashes
   - `test_render_with_brace_characters_in_values`: Values containing `{` and `}`
   - `test_render_with_null_bytes_in_values`: Control characters in values

3. **Unicode and Internationalization**
   - `test_render_with_unicode_emoji`: Emoji characters (🐛, 🚀, ✨)
   - `test_render_with_unicode_emoji`: CJK characters (日本語, 中文, 한국ة, العربية)

4. **Large Values**
   - `test_render_with_very_long_values`: 10,000 character values
   - `test_performance_large_template`: Templates with 100 placeholders

5. **Repeated Patterns**
   - `test_render_with_multiple_occurrences_of_same_placeholder`: Same placeholder repeated
   - `test_render_with_interleaved_placeholders_and_text`: Mixed placeholders and text

### Error Path Tests

Tests for invalid inputs and failure modes:

1. **Missing/Invalid Placeholders**
   - `test_render_with_missing_placeholder_value`: Empty string for missing values
   - `test_render_with_unknown_placeholder`: Unknown placeholders remain unrendered

2. **Malformed Templates**
   - `test_extract_placehandles_from_malformed_template`: Missing braces
   - `test_extract_placeholders_with_nested_braces`: Nested brace handling
   - `test_extract_placeholders_with_special_chars`: Invalid placeholder names
   - `test_backend_validate_with_invalid_placeholders`: Disallowed placeholders
   - `test_backend_validate_with_malformed_placeholder`: Unclosed braces

3. **Load Failures**
   - `test_load_backend_with_invalid_yaml`: Invalid YAML structure
   - `test_load_backend_with_missing_required_fields`: Incomplete backend descriptors
   - `test_load_backend_with_empty_name`: Empty backend name
   - `test_load_backend_with_invalid_regex`: Invalid identity_pattern regex
   - `test_backend_validate_with_empty_operation_name`: Empty operation names

4. **Variable Conflicts**
   - `test_render_with_vars_empty_extra_vars`: Empty extra variables
   - `test_render_with_vars_conflicting_names`: Extra vars overriding context

### Integration Tests - Full Pipeline

Tests for the complete descriptor → render pipeline:

1. **Bead-RS Backend**
   - `test_full_pipeline_render_with_bead_rs_backend`: Full show operation
   - `test_full_pipeline_render_with_multiple_placeholders`: Claim operation
   - `test_full_pipeline_render_all_bead_rs_operations`: All operations renderable

2. **User-Defined Backends**
   - `test_full_pipeline_load_and_render_user_backend`: Custom backend from YAML
   - `test_full_pipeline_builtin_override`: User backends overriding builtins

### Performance/Benchmark Tests

Tests ensuring rendering performance meets requirements:

1. **Simple Templates**
   - `test_performance_simple_template`: 1,000 renders < 100ms
   - Template: "Task: {bead_title} in {workspace}"

2. **Complex Templates**
   - `test_performance_complex_template`: 1,000 renders < 200ms
   - 6 placeholders, longer values

3. **Placeholder Extraction**
   - `test_performance_extract_placeholders`: 1,000 extractions < 50ms

4. **Large Templates**
   - `test_performance_large_template`: 100 renders with 100 placeholders < 500ms

## Placeholder Types

The template system supports these placeholder types:

### Standard Placeholders (RenderContext)
- `{bead_id}` - Bead identifier
- `{bead_title}` - Bead title
- `{bead_body}` - Bead description (falls back to "(no description)")
- `{bead_status}` - Bead status (open, in_progress, done, blocked, deferred)
- `{bead_priority}` - Priority level (1-4)
- `{bead_assignee}` - Current assignee (falls back to "(unassigned)")
- `{bead_labels}` - Comma-separated labels
- `{workspace}` - Workspace path
- `{worker_id}` - Worker identifier
- `{created_at}` - Creation timestamp (UTC)
- `{updated_at}` - Last update timestamp (UTC)
- `{actor}` - Alias for bead_assignee

### CLI Operation Placeholders (Backend Descriptor)
Backend-specific placeholders allowed per operation:

- `ready`: `{limit}`, `{assignee}`
- `list_all`: `{limit}`
- `show`, `release`, `block`, `clear_assignee`, `reopen`, `labels`, `why`: `{id}`
- `claim`: `{id}`, `{actor}`
- `claim_auto`: `{actor}`, `{model}`, `{harness}`, `{harness_version}`
- `label_add`, `label_remove`: `{id}`, `{label}`
- `create`: `{title}`, `{body}`, `{priority}`, `{assignee}`, `{issue_type}`, `{labels}`
- `dep_add`, `dep_remove`: `{blocked}`, `{blocker}`
- `split`: `{parent}`, `{children}`
- `close`: `{id}`, `{reason}`
- `import`: `{input}`, `{mode}`, `{actor}`
- `compare`: `{id}`, `{profile}`
- `query`: `{query}`
- `changes`: `{since}`
- `ref_add`: `{id}`, `{namespace}`, `{key}`, `{value}`
- `ref_remove`: `{id}`, `{namespace}`, `{key}`
- `ref_list`: `{id}`
- `ref_find`: `{namespace}`, `{value}`
- `data_set`: `{id}`, `{key}`, `{value}`
- `data_get`, `data_list`, `data_remove`: `{id}`, `{key}`
- `recurrence_add`: `{template}`, `{schedule}`
- `recurrence_remove`: `{id}`
- `recurrence_list`, `policy_validate`, `flush`, `doctor_check`, `doctor_repair`, `create_id`: (none)

## Running the Tests

### Run All Template Tests
```bash
cargo test template
```

### Run Specific Test Suite
```bash
# Unit tests only
cargo test --lib template

# Integration tests only
cargo test --test template_rendering_tests

# Comprehensive tests only
cargo test --test template_comprehensive_tests
```

### Run Performance Tests
```bash
cargo test --test template_comprehensive_tests performance
```

### Run with Output
```bash
cargo test template -- --nocapture
```

## Test Coverage Goals

The comprehensive test suite aims to achieve:

- ✅ **100% placeholder type coverage**: All standard and operation-specific placeholders tested
- ✅ **Edge case coverage**: Empty strings, special characters, unicode, large values
- ✅ **Error path coverage**: Invalid inputs, malformed templates, load failures
- ✅ **Integration coverage**: Full pipeline from descriptor to rendered output
- ✅ **Performance coverage**: Benchmarks for common use cases

## Maintenance

When adding new placeholder types:
1. Update the placeholder lists in this documentation
2. Add tests in `template_comprehensive_tests.rs` for the new placeholder
3. Add integration tests in `template_rendering_tests.rs` for operations using the placeholder
4. Verify performance tests still pass

When modifying template rendering logic:
1. Run full test suite: `cargo test template`
2. Ensure all edge case tests pass
3. Verify performance benchmarks meet requirements
4. Update this documentation if behavior changes

## Known Limitations

1. **Nested braces**: The current implementation extracts `{id_{inner}}` as a single placeholder named `id_{inner}` rather than supporting true nesting
2. **Unknown placeholders**: Unknown placeholders remain as-is (e.g., `{unknown}` stays `{unknown}` in output)
3. **Performance benchmarks**: Timings are calibrated for the development hardware; CI environments may vary

## Related Documentation

- `src/template.rs` - Template rendering implementation
- `src/bead_store/backend.rs` - Backend descriptor system
- `CLAUDE.md` - Project conventions (testing section)
- `docs/testing-isolation-patterns.md` - Test isolation guidelines
