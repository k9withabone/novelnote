# NovelNote

NovelNote is a self-hosted book tracker. Use it to keep track of books you have read and those you want to read.

## Status

NovelNote is currently a work-in-progress.

## Planned Features

- [ ] Multi user
  - [ ] OIDC login
  - [ ] Password login
    - [ ] Two-factor via TOTP
    - [ ] Password reset via CLI
  - [ ] Display name
  - [ ] Gravatar avatars
  - [ ] Public SSH keys
- [ ] Libraries
  - Books are grouped into libraries.
  - A library is owned by a user and other users can be added to it.
  - [ ] Library users
  - [ ] Library user roles
    - Read: view books, authors, genres, etc.
    - Write: create/edit books
    - Admin: delete books, manage library users
    - Owner: transfer ownership, delete library
  - [ ] Copy items between libraries
  - [ ] Bulk import books
    - [ ] CSV
    - [ ] [audiobookshelf](https://github.com/advplyr/audiobookshelf)
    - [ ] Goodreads
  - [ ] Export as CSV
- [ ] Books
  - [ ] People
    - Authors, narrators, publishers
  - [ ] Genres
  - [ ] Tags
  - [ ] Series
    - A book can be in multiple series.
  - [ ] Import metadata
    - [ ] Search via:
      - [ ] Title and optional author
      - [ ] ISBN
      - [ ] ASIN
    - [ ] Sources:
      - [ ] Open Library
      - [ ] Amazon/Audible
      - [ ] Google books
      - [ ] Goodreads
  - [ ] User progress and notes
- [ ] User collections
  - List of books or series from any library.
  - Can be used to create ordered "To Be Read" lists.
- [ ] Configuration
  - All configuration is external. No overall "admin" user that controls server settings.
  - From CLI, Environment, and TOML file.
- [x] Logging
  - [x] stdout
  - [x] Rotating files
  - [x] journald
- [ ] Graceful shutdown
- [ ] SSH interfaces
  - [ ] CLI
  - [ ] TUI
- [ ] systemd integration
  - [ ] Service `Type=notify` support
  - [ ] Socket activation with configurable timeout

## Packages

Development of NovelNote is separated into several packages.

| Package | Description |
| ------- | ----------- |
| `novelnote` (top-level) | The main server binary. Provides the CLI, loads configuration, and starts the server. |
| [`novelnote_server`](./server) | The server library. Provides the HTTP server implementation. |

## License

All source code for NovelNote is licensed under the [Mozilla Public License v2.0](https://www.mozilla.org/en-US/MPL/).
View the [LICENSE](./LICENSE) file for more information.
