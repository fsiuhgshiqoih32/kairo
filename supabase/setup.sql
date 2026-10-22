-- Kairo friends: who you are, your friends, and the clips and messages you send them.
--
-- Paste all of this into your Supabase project's SQL Editor and press Run. Running it again is
-- safe: it only adds what's missing and puts the rules back the way they are here.
--
-- Kairo signs people in with Google through Supabase Auth, so every Kairo user has the
-- `authenticated` role. The rules below decide what each of them may see and change: your own
-- profile, friendships you're part of, and messages you sent or received.
-- Clips themselves aren't stored here, only links to them (Kairo uploads clips to catbox.moe).

-- 1. Profiles: the username friends find you by.
create table if not exists public.profiles (
  id uuid primary key default auth.uid() references auth.users (id) on delete cascade,
  username text not null unique check (username ~ '^[a-z0-9_]{3,20}$'),
  created_at timestamptz not null default now()
);

-- 2. Friendships: a request from one person, accepted by the other.
create table if not exists public.friendships (
  requester uuid not null default auth.uid() references public.profiles (id) on delete cascade,
  addressee uuid not null references public.profiles (id) on delete cascade,
  accepted boolean not null default false,
  created_at timestamptz not null default now(),
  primary key (requester, addressee),
  check (requester <> addressee)
);
-- One friendship per pair, whoever asked first.
create unique index if not exists friendships_pair
  on public.friendships (least(requester, addressee), greatest(requester, addressee));
create index if not exists friendships_addressee on public.friendships (addressee);

-- 3. Messages between friends: some text, a clip link, or both.
create table if not exists public.messages (
  id bigint generated always as identity primary key,
  sender uuid not null default auth.uid() references public.profiles (id) on delete cascade,
  recipient uuid not null references public.profiles (id) on delete cascade,
  body text not null default '' check (char_length(body) <= 2000),
  clip_url text,
  clip_title text check (char_length(clip_title) <= 200),
  clip_seconds real check (clip_seconds >= 0),
  created_at timestamptz not null default now(),
  read_at timestamptz,
  check (body <> '' or clip_url is not null)
);
-- A clip is a Kairo share link: its page, or the file on one of the hosts Kairo uses.
alter table public.messages drop constraint if exists messages_clip_url_check;
alter table public.messages add constraint messages_clip_url_check check (
  clip_url ~ '^https://(kairo-clips\.bfs-worker\.workers\.dev/c/[A-Za-z0-9]{8}|(files|litter)\.catbox\.moe/[A-Za-z0-9._-]+|pomf2\.lain\.la/f/[A-Za-z0-9._-]+)$'
);
create index if not exists messages_recipient on public.messages (recipient, id);
create index if not exists messages_sender on public.messages (sender, id);

-- Helpers the rules use. They live in a schema the API doesn't expose, so nobody can call them
-- directly.
create schema if not exists private;
grant usage on schema private to authenticated;

create or replace function private.are_friends(a uuid, b uuid) returns boolean
language sql stable security definer set search_path = '' as $$
  select exists (
    select 1 from public.friendships f
    where f.accepted
      and ((f.requester = a and f.addressee = b) or (f.requester = b and f.addressee = a))
  );
$$;
revoke execute on function private.are_friends(uuid, uuid) from public;
grant execute on function private.are_friends(uuid, uuid) to authenticated;

-- Limits, so one account can't flood everyone: 30 messages a minute and 1000 a day, 30 friend
-- requests an hour.
create or replace function private.limit_messages() returns trigger
language plpgsql security definer set search_path = '' as $$
begin
  if (select count(*) from public.messages m
      where m.sender = new.sender and m.created_at > now() - interval '1 minute') >= 30
     or (select count(*) from public.messages m
      where m.sender = new.sender and m.created_at > now() - interval '1 day') >= 1000 then
    raise exception 'Too many messages. Wait a moment and try again.';
  end if;
  return new;
end $$;
revoke execute on function private.limit_messages() from public;
drop trigger if exists messages_limit on public.messages;
create trigger messages_limit before insert on public.messages
  for each row execute function private.limit_messages();

create or replace function private.limit_requests() returns trigger
language plpgsql security definer set search_path = '' as $$
begin
  if (select count(*) from public.friendships f
      where f.requester = new.requester and f.created_at > now() - interval '1 hour') >= 30 then
    raise exception 'Too many friend requests. Try again in an hour.';
  end if;
  return new;
end $$;
revoke execute on function private.limit_requests() from public;
drop trigger if exists friendships_limit on public.friendships;
create trigger friendships_limit before insert on public.friendships
  for each row execute function private.limit_requests();

-- Who may touch what. Nothing for signed-out callers; signed-in users get exactly these, and
-- may change only one column each: their username, accepting a request, marking a message read.
alter table public.profiles enable row level security;
alter table public.friendships enable row level security;
alter table public.messages enable row level security;

revoke all on public.profiles, public.friendships, public.messages from anon, authenticated;
grant select, insert, delete on public.profiles, public.friendships, public.messages to authenticated;
grant update (username) on public.profiles to authenticated;
grant update (accepted) on public.friendships to authenticated;
grant update (read_at) on public.messages to authenticated;

-- Profiles: anyone signed in can look a username up; only you can make or change yours.
drop policy if exists "profiles: read" on public.profiles;
create policy "profiles: read" on public.profiles for select to authenticated using (true);
drop policy if exists "profiles: create your own" on public.profiles;
create policy "profiles: create your own" on public.profiles for insert to authenticated
  with check (id = (select auth.uid()));
drop policy if exists "profiles: change your own" on public.profiles;
create policy "profiles: change your own" on public.profiles for update to authenticated
  using (id = (select auth.uid())) with check (id = (select auth.uid()));
drop policy if exists "profiles: delete your own" on public.profiles;
create policy "profiles: delete your own" on public.profiles for delete to authenticated
  using (id = (select auth.uid()));

-- Friendships: you see the ones you're in, ask in your own name, accept what was asked of you,
-- and can end any of yours.
drop policy if exists "friendships: read yours" on public.friendships;
create policy "friendships: read yours" on public.friendships for select to authenticated
  using ((select auth.uid()) in (requester, addressee));
drop policy if exists "friendships: ask" on public.friendships;
create policy "friendships: ask" on public.friendships for insert to authenticated
  with check (requester = (select auth.uid()) and not accepted);
drop policy if exists "friendships: accept" on public.friendships;
create policy "friendships: accept" on public.friendships for update to authenticated
  using (addressee = (select auth.uid())) with check (addressee = (select auth.uid()) and accepted);
drop policy if exists "friendships: end" on public.friendships;
create policy "friendships: end" on public.friendships for delete to authenticated
  using ((select auth.uid()) in (requester, addressee));

-- Messages: you see what you sent and got, send only to friends, mark what you got as read,
-- and can take back what you sent.
drop policy if exists "messages: read yours" on public.messages;
create policy "messages: read yours" on public.messages for select to authenticated
  using ((select auth.uid()) in (sender, recipient));
drop policy if exists "messages: send to friends" on public.messages;
create policy "messages: send to friends" on public.messages for insert to authenticated
  with check (sender = (select auth.uid()) and private.are_friends(sender, recipient));
drop policy if exists "messages: mark read" on public.messages;
create policy "messages: mark read" on public.messages for update to authenticated
  using (recipient = (select auth.uid())) with check (recipient = (select auth.uid()));
drop policy if exists "messages: take back" on public.messages;
create policy "messages: take back" on public.messages for delete to authenticated
  using (sender = (select auth.uid()));

-- Let the API see the new tables straight away.
notify pgrst, 'reload schema';
