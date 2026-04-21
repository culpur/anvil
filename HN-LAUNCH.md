# Hacker News Launch — Draft

**Posting window:** Tuesday or Wednesday, 6am–8am Pacific (highest daytime front-page traffic).
**Account:** Post from an established HN account with some karma. New accounts with 0 karma posting "Show HN" land softly. If needed, submit the post from a 2+ year old account with a non-spam history.
**Self-comment:** Have the first comment ready to post 30 seconds after the submission. Good comments make the post. Don't leave an empty thread.

---

## Option A — Show HN title (recommended)

> **Show HN: Anvil – An AI coding assistant that isn't locked to one provider**

Why this works: "Show HN" signals it's a real thing you can download, not a marketing post. "Isn't locked to one provider" is the one claim that differentiates and that HN cares about.

## Option B — Alternative title (if Option A feels too soft)

> **Show HN: Anvil – Hand your AI coding session to any browser**

Why this works: Remote control is the single feature no competitor has. Title focuses on the demo-able thing. Downside: makes the post feel narrower than the actual product.

**My lean:** Option A. The freedom story is the bigger story; the remote-control story is the hook we reveal in the body.

---

## Body text (goes in the main post if using `text` submission, or as a linked comment if using `url` submission)

HN allows either a URL submission or a text submission. For a product launch I'd use **URL submission pointing at https://culpur.net/anvil** and then post the body below as the first comment within a minute of submitting.

---

Hey HN. I built Anvil because every AI coding tool I tried came with a leash.

Claude Code locks you to Anthropic. Copilot to GitHub. Cursor to their wrapper and their pricing. Your prompts, your code, your costs — all flow through one vendor's pipes, and if that vendor changes terms or pricing, you're stuck.

Anvil is the opposite. One binary, five providers (Anthropic, OpenAI, Google, xAI, local Ollama), your own API keys. Switch mid-conversation. When one rate-limits, fall over to the next. When one gets expensive, change it. Zero telemetry. No account required to use it.

**The one thing no other tool does:** type `/remote-control` in your terminal and you get a URL + a 6-digit pairing code. Open the URL on any device, enter the code, and you've got full bidirectional control of the session. Not a read-only transcript — you can type, run commands, manage tabs, swap providers, all from your phone while your workstation does the heavy lifting. I built this because I wanted to keep a long session going while I walked my dog. Turned out other people want it too.

A few honest things:

- It's ~1 month in market. I'm a solo founder with a day job. Pace will be steady, not furious.
- Right now it's 100% free. No paywall, no tier system. I'll introduce a paid hosted-compute option for people who want to run Ollama on my infrastructure instead of theirs, but the local-keys-only path will stay free.
- The encrypted vault (AES-256-GCM + Argon2id) handles 21 credential types. Per-project scopes. Nothing touches disk unencrypted.
- Tab sandboxing means each tab has its own model, provider, and credential scope. Good for consultants juggling client credentials.
- It's a Rust TUI with a companion web viewer. 15 MB binary. Runs on macOS, Linux, Windows.

Links:
- Product page: https://culpur.net/anvil
- GitHub releases: https://github.com/culpur/anvil/releases
- Marketplace (Skills / Plugins / Agents / Themes): https://anvilhub.culpur.net

Happy to answer anything. Specific things I'd like feedback on: the remote-control pairing flow (is 6 digits + ephemeral URL enough? Should there be TOTP?), the README positioning, and whether the `/remote-control` + multi-provider story resonates as much as I think it does.

---

## Prepared responses to likely comments

HN comments will almost certainly hit these angles. Have answers ready before posting.

### "Why not just use [existing tool]?"

Every existing tool locks you to one provider's business model. If Anthropic changes its terms (as it did in early 2026), your Claude Code workflow changes too. If Copilot raises prices, GitHub users eat it. Anvil treats providers as interchangeable; your workflow survives vendor decisions. The cost of that freedom is you managing your own keys — which you probably want to do anyway.

### "Proprietary license, not open source?"

Correct. Binaries are free to download and use. Source is not public right now. I'm a solo founder; I want the option to build paid tiers around hosted compute without handing competitors a free fork. The binary is reproducible from tagged releases and heavily tested (500+ tests across the workspace). If that's a dealbreaker for you, I get it.

### "What about [niche provider / local model]?"

Ollama covers basically any local model (Llama, Qwen, Mistral, DeepSeek, Gemma, Phi, whatever you can run). Cloud providers are currently Anthropic, OpenAI, Google, xAI. Adding more is a matter of the provider having an OpenAI-compatible or documented API; open an issue with what you want.

### "How do you make money?"

Right now: I don't. v2.2.6 is Model 1 — fully self-contained, nothing goes to my infrastructure, no monetization. Soon: a hosted-Ollama option for people who don't want to run inference locally but also don't want their prompts going to US-based cloud providers. After that: a BYOK platform fee for people who want orchestration across their own keys without running their own relay. Enterprise managed pools much later. The free tier stays free.

### "The remote control thing sounds insecure"

Fair concern. It works like this: your instance generates an ephemeral URL + 6-digit pairing code. The code + URL together let one browser pair with your session over an encrypted WebSocket relay. The relay doesn't hold state beyond the live session — nothing persists. If you kill the terminal, the session dies. You can also run your own relay server instead of using the Culpur-hosted one; all the code for that is in the repo. Is 6 digits enough? Arguable. I'm considering adding TOTP as an option.

### "Windows support?"

Yes, binary ships for Windows x86_64. TUI works, web viewer works, credential vault works. Self-respawn after plugin install isn't supported on Windows — it prompts you to relaunch manually instead. macOS/Linux is the primary target; Windows is tested but less polished.

### "Benchmarks vs Claude Code / Cursor?"

No formal benchmarks yet. Anvil is a UI + orchestration layer; raw inference speed is whatever the underlying provider gives you. What I can say: for local Ollama setups, Anvil adds negligible overhead (~50ms per turn for relay + vault ops). For cloud providers, cost depends entirely on which provider you pick. Anvil tracks per-session token usage and dollar cost against your configured budget.

### "I want X feature"

File an issue. Solo founder, so pace is what it is, but the next planned features are session persistence (v2.3) and cloud-executed recurring agents (v2.4 "Routines"). Full roadmap on the GitHub repo.

---

## Crosspost channels (sequenced, not simultaneous)

**Day 1 (HN launch day):**
- r/LocalLLaMA — angle: "How I use local Ollama with 5-provider failover"
- Twitter/X: lead with 30s gif of the /remote-control demo (phone controlling laptop)

**Day 2–3:**
- r/selfhosted — angle: "Self-host your AI coding assistant (bring your own Ollama)"
- Hackernewsletter / TLDR submission

**Day 4–5:**
- r/devops — angle: "AI coding with per-project credential vault and egress allowlist"
- Dev Discord communities (ThePrimeagen's, Fireship, etc.)

**Day 7+:**
- Follow-up HN post if Day 1 underperforms — different angle (remote control or vault)

---

## What "success" looks like on Day 1

- 50+ upvotes = front page for a few hours, a few hundred downloads
- 200+ upvotes = front page top half, a few thousand downloads, inbound inquiries
- 500+ upvotes = several thousand downloads, potentially press pickup, enterprise inbound

Realistic target: 50–100 upvotes. Solo-founder projects with no pre-existing community rarely hit 200+ on first try. The goal is establishing Anvil's existence with a skeptical technical audience, not going viral.

**What NOT to do:**
- Don't edit the post after submission (looks spammy)
- Don't reply to every comment immediately (looks desperate)
- Don't argue with detractors (let other commenters do that)
- Don't drop the post and disappear — stay in the thread for the first 3 hours answering thoughtfully

---

## Pre-launch checklist

- [ ] README published with freedom-first positioning (done: commit e483cfa)
- [ ] culpur.net/anvil page matches README positioning (done: page 619 updated)
- [ ] anvilhub.culpur.net homepage and /about page show v2.2.6 (done)
- [ ] Working download links for all 5 platforms (verified in last anvil_verify run)
- [ ] Homebrew formula points at v2.2.6 (verified)
- [ ] GitHub release v2.2.6 notes are real, not "release: v2.2.6" auto-slop (done: already rewrote on 2026-04-20)
- [ ] 30s gif of `/remote-control` demo captured for Twitter/X
- [ ] HN account chosen (karma > 100, no recent bans, history visible)
- [ ] Draft first-comment response posted within 60 seconds of the submission
- [ ] Decide submission time (Tue/Wed 6–8am Pacific)
