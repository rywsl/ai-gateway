create table if not exists gemini_file_mapping (
    sha256_hex text not null,
    user_id text not null,
    file_uri text not null,
    mime_type text not null,
    size_bytes bigint not null,
    expire_unix_s bigint not null,
    created_at timestamptz not null default now(),
    primary key (sha256_hex, user_id)
);

create index if not exists idx_gemini_file_mapping_expire on gemini_file_mapping (expire_unix_s);
