# ShardBrowser Launcher

Desktop app for managing anti-detect browser identities: creating them, binding proxies, editing fingerprints and launching them in the patched engine.

## Language

**Profile**:
A saved browser identity: fingerprint, proxy binding, cookies and storage, launched on demand.
_Avoid_: Browser, account, identity

**Session**:
One live Engine process running a Profile. Starting a Profile opens a Session; stopping it ends the Session. A Profile with a Session shows as "Running".
_Avoid_: Browser, instance, window

**Engine**:
The patched Chromium build that ShardBrowser downloads and launches Profiles in.
_Avoid_: Browser, Chrome, core

## Organizing Profiles

**Folder**:
A named, colored group of Profiles, usually one per client or project. A Profile belongs to at most one Folder. A group of Bookmarks is always called a Bookmark folder, never a bare Folder.
_Avoid_: Space, group, workspace

**Pinned Profile**:
A Profile kept in the sidebar's Pinned section so it can be started or focused in one click from anywhere.
_Avoid_: Favorite, starred

**Tag**:
A colored label attached to a Profile to mark a cross-cutting state such as "warm-up" or "needs 2FA". A Profile can carry many Tags.
_Avoid_: Label, marker

**View**:
A saved filter, sort and column set (which columns show, in what order) over Profiles, pinned in the sidebar. A View holds no Profiles itself; it only selects them.
_Avoid_: Filter, smart folder, preset

**Profile template**:
A Profile's settings kept under a name (proxy, WebRTC, noise, extensions, Tags, Folder and the rest) that "New profile" can start from; one can be the default. It keeps where the fingerprint came from, never the fingerprint, name, notes or cookies: each Profile made from it gets a fingerprint of its own. In a team it is the team's.
_Avoid_: Preset, blueprint, fingerprint template

## Interface

**Peek**:
The side panel that shows and edits one Profile while the Profile list stays visible.
_Avoid_: Drawer, inline editor, details page

## Sync

**Sync**:
A person's own Profiles, proxies and the rest kept the same on every computer they use, through a team server: their own or a joined team's. A synced Profile is open on one computer at a time.
_Avoid_: Cloud, backup, synchronize; "Start in sync" is another thing (windows driven together). In Russian and Chinese it says "between computers" (между компьютерами, 在电脑间) where both could meet.

**Open on (a computer)**:
A synced Profile another of the person's computers holds: Start is refused here until it closes there, or until "Open here instead" takes it.
_Avoid_: Locked, busy, in use

**Conflict copy**:
The Profile a sync makes when one was changed on two computers: this computer's changes, beside the original. Both share one fingerprint and cookies, so they are not run together.
_Avoid_: Duplicate, clone

## Words in the interface

How the glossary reaches the screen. The lines marked *checked* have a rule in `scripts/lib/copy-rules.mjs`, run by `npm run check:i18n`.

- **Case.** Sentence case in every string: buttons, menu items, titles, Cmd+K commands. Glossary nouns are common nouns in running text ("Save as view", "Move to folder", "All profiles", "3 profiles"). Three places keep a capital because the screen names them that way: **Trash**, **Settings**, **Peek**. *Checked.*
- **Browser and browser engine.** "Browser" is the window a Profile opens ("Open the browser"). "Browser engine" is what ShardBrowser downloads and updates (the Engine above). Never "core". *Checked.*
- **Fingerprint.** Always "fingerprint", never "template" ("Fingerprint", "Pick a library fingerprint"). "Template" names a Profile template only, in its own keys (`templates.*`). *Checked.*
- **This computer.** The user's own machine is "this computer" and its screen "this computer's screen"; never "this machine", "your machine" or "the host". *Checked.*
- **Local API.** The local HTTP API is the "Local API", so it doesn't clash with the Automation page (Projects). *Checked.*
- **Help.** One line of help per control; the rest goes to docs behind "Learn more about {topic}", never a bare "Learn more". *Checked.*
- **Spelling and punctuation.** US English. Curly quotes and apostrophes. An ellipsis only on a command that asks for more before it acts. No arrows or check marks in labels. No middle dot anywhere, in English, Chinese, Russian or the What's new notes. Full plural templates. *Checked.*
- **Chinese.** 配置 Profile, 代理 proxy, 回收站 Trash, 文件夹 Folder, 标签 Tag, 视图 View, 指纹 fingerprint, 模板 Profile template, 预览 Peek, 置顶 Pin, 订单 order, 国家/地区 country; 你, never 您. The What's new notes are English; the Chinese dialog says so in one line (`whatsNew.englishOnly`). *Checked.*
- **Russian.** профиль Profile, прокси proxy (it never changes form), Корзина Trash (a capital К mid-sentence too), папка Folder, тег Tag, сохранённый фильтр View (the sidebar group and the Settings tab are Сохранённые фильтры; never представление), отпечаток fingerprint (never шаблон), шаблон Profile template, быстрый просмотр Peek, закрепить and открепить Pin and Unpin, заказ order, движок браузера browser engine (never ядро), резидентские прокси residential proxies (never резидентные). Verbs: Запустить and Остановить for Start and Stop, Назначить for Assign, Убрать for Remove, Удалить for Delete. «вы», lowercase, never «ты»; quotes are «ёлочки». A count's word takes three forms, written where the word goes as `{n|профиль|профиля|профилей}` (one, few, many); the number in `n` picks one, so the English `…One` and plural keys still work for 21 and 22. *Checked.*

### UI verbs

One verb per action, the same in the menu, the button, the dialog, the toast and Cmd+K.

| Verb | Used for | The toast after it | Never |
|---|---|---|---|
| **Start**, **Stop** | a Profile's browser | Started “Sandbox” | Launch, Run |
| **Assign** | a proxy to profiles | Assigned proxies to 3 profiles | Bind, Distribute, Use |
| **Move to Trash** | profiles (they can come back for 7 days) | Moved “Sandbox” to Trash, with Undo | Delete (for a profile) |
| **Restore** | a profile from Trash | Restored “Sandbox” | Recover, Undelete |
| **Delete for good** | a profile in Trash | Deleted “Sandbox” for good | Purge |
| **Delete** | things with no Trash: proxies, bookmarks, tags, views, folders, projects | Deleted proxy “DC Tokyo”, with Undo | Remove (for these) |
| **Remove** | an item from a library or a set it still exists outside of: an extension, a library fingerprint, a tag on a profile | Removed “uBlock Origin Lite” | Delete (for these) |
| **Update** | an extension to a newer version from the Web Store | Updated “MetaMask” to 13.53.0 | Upgrade, Refresh |
| **Move to folder** | profiles into a Folder | Moved 3 profiles to “Ads” | Put, Assign |
| **Pin**, **Unpin** | a Profile in the sidebar | Pinned “Sandbox” | Favorite, Star |
| **Copy as JSON** | profiles or proxies to the clipboard | Copied 3 profiles as JSON | Export (for the clipboard) |
| **Import from clipboard** | profiles or proxies from copied JSON or lines | Imported 3 profiles | Import (alone, for the clipboard) |
