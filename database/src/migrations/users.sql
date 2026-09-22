CREATE TABLE users(
    id BLOB PRIMARY KEY,
    username TEXT NOT NULL UNIQUE,
    password_hash TEXT,
    openid TEXT UNIQUE,
    email TEXT UNIQUE,
    email_verified INTEGER NOT NULL DEFAULT 0,
    display_name TEXT,
    CHECK (password_hash IS NOT NULL OR openid IS NOT NULL),
    CHECK (
        CASE WHEN email_verified IS TRUE THEN
            email IS NOT NULL
        ELSE
            TRUE
        END
    )
) STRICT;

CREATE TRIGGER user_email_verified BEFORE UPDATE OF email ON users
    WHEN new.email != old.email OR new.email IS NULL
BEGIN
    UPDATE test SET email_verified = 0 WHERE rowid = old.rowid;
END;
