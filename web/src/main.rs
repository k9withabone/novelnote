//! `novelnote_web` is the web frontend for NovelNote, a self-hosted book tracker.

use leptos::{html::ElementChild, mount::mount_to_body, view};

fn main() {
    mount_to_body(|| view! { <p>"Hello, world!"</p> });
}
