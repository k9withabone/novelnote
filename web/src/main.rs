//! `novelnote_web` is the web frontend for NovelNote, a self-hosted book tracker.

use leptos::{attr::global::ClassAttribute, html::ElementChild, mount::mount_to_body, view};

fn main() {
    mount_to_body(|| {
        view! {
            <main class="flex justify-center items-center h-screen text-black dark:text-white bg-slate-50 dark:bg-slate-800">
                <h1 class="text-6xl font-bold sm:text-8xl">"NovelNote"</h1>
            </main>
        }
    });
}
