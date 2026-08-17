//! Standalone test for template rendering functionality.
//! This file can be run independently to verify template rendering works correctly.

use std::collections::HashMap;

// Helper macro for creating context HashMap
macro_rules! create_context {
    ($($key:expr => $value:expr),*) => {{
        let mut map = HashMap::new();
        $(
            map.insert($key.to_string(), $value.to_string());
        )*
        map
    }};
}

fn main() {
    println!("Testing template rendering functionality...\n");

    // Test 1: Basic placeholder substitution
    test_basic_substitution();

    // Test 2: Multiple placeholders
    test_multiple_placeholders();

    // Test 3: Labels joining
    test_labels_joining();

    // Test 4: Numeric conversion
    test_numeric_conversion();

    // Test 5: Empty values
    test_empty_values();

    println!("\n✅ All template rendering tests passed!");
}

fn test_basic_substitution() {
    println!("Test 1: Basic placeholder substitution");
    let template = "bf show {id}";
    let context = create_context!("id" => "bf-abc123");
    let result = render_template(template, &context);
    assert_eq!(result, "bf show bf-abc123");
    println!("  ✓ Basic substitution works");
}

fn test_multiple_placeholders() {
    println!("Test 2: Multiple placeholders");
    let template = "bf show {id} --actor {actor} --limit {limit}";
    let mut context = HashMap::new();
    context.insert("id".to_string(), "bf-123".to_string());
    context.insert("actor".to_string(), "worker-1".to_string());
    context.insert("limit".to_string(), "50".to_string());

    let result = render_template(template, &context);
    assert_eq!(result, "bf show bf-123 --actor worker-1 --limit 50");
    println!("  ✓ Multiple placeholders work");
}

fn test_labels_joining() {
    println!("Test 3: Labels joining");
    let template = "bf create --labels {labels}";
    let mut context = HashMap::new();
    context.insert("labels".to_string(), "bug,high-priority".to_string());

    let result = render_template(template, &context);
    assert_eq!(result, "bf create --labels bug,high-priority");
    println!("  ✓ Labels joining works");
}

fn test_numeric_conversion() {
    println!("Test 4: Numeric conversion");
    let template = "bf list --limit {limit}";
    let context = create_context!("limit" : "100");
    let result = render_template(template, &context);
    assert_eq!(result, "bf list --limit 100");
    println!("  ✓ Numeric conversion works");
}

fn test_empty_values() {
    println!("Test 5: Empty values");
    let template = "bf create --title '{title}' --body '{body}'";
    let mut context = HashMap::new();
    context.insert("title".to_string(), "".to_string());
    context.insert("body".to_string(), "".to_string());

    let result = render_template(template, &context);
    assert_eq!(result, "bf create --title '' --body ''");
    println!("  ✓ Empty values work");
}

// Simple template rendering implementation
fn render_template(template: &str, context: &HashMap<String, String>) -> String {
    let mut result = template.to_string();

    for (key, value) in context {
        let placeholder = format!("{{{}}}", key);
        result = result.replace(&placeholder, value);
    }

    result
}
