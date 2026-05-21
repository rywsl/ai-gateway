create table if not exists model_mapping (
    user_alias text not null,
    protocol text not null,
    default_upstream_model text not null,
    vendor_key_overrides jsonb not null default '{}'::jsonb,
    availability text not null default 'enabled',
    pricing_strategy_ref text,
    updated_at timestamptz not null default now(),
    primary key (user_alias, protocol)
);
