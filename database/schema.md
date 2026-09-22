# NovelNote Database Schema

Documentation of the schema used by NovelNote for its [SQLite](https://sqlite.org/) database.

Each heading is the name of a database table. All columns in the table are listed below and include
the used SQLite type, any column constraints, and the Rust type the column maps to. The column name
matches the field name used in Rust structs. Any table constraints are then listed. All tables use
the [STRICT](https://sqlite.org/stricttables.html) table option. Next are any indexes, views, and/or
notes for the table. Finally, if any of the columns are mapped to custom Rust types, they are shown.

## users

| Column         | SQLite Type                | Rust Type                 |
| -------------- | -------------------------- | ------------------------- |
| id             | BLOB PRIMARY KEY           | `Uuid`                    |
| username       | TEXT UNIQUE NOT NULL       | `Username`                |
| password_hash  | TEXT                       | `Option<String>`          |
| openid         | TEXT UNIQUE                | `Option<String>`          |
| email          | TEXT UNIQUE                | `Option<lettre::Address>` |
| email_verified | INTEGER NOT NULL DEFAULT 0 | `bool`                    |
| display_name   | TEXT                       | `Option<Name>`            |

User has to be able to log in with a password or via OIDC:
```sql
CHECK (password_hash IS NOT NULL OR openid IS NOT NULL)
```

Email can't be verified if it's NULL:
```sql
CHECK (
    CASE WHEN email_verified IS TRUE THEN
        email IS NOT NULL
    ELSE
        TRUE
    END
)
```

Ensure `email_verified` is set to `false` when the email is updated or removed:
```sql
CREATE TRIGGER user_email_verified BEFORE UPDATE OF email ON users
    WHEN new.email != old.email OR new.email IS NULL
BEGIN
    UPDATE test SET email_verified = 0 WHERE rowid = old.rowid;
END;
```

Custom Rust types:
```rust
// Not empty, POSIX compatible username, see the useradd(8) man page.
// POSIX compatibility needed for SSH capabilities to be added later.
struct Username(String);

// Not empty, no new lines.
struct Name(String);
```

