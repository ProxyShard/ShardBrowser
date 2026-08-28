"""End-to-end: create a persistent profile, configure anti-fingerprint noise,
launch + drive it, then reopen it later. The same fingerprint and browser state
come back; delete it when you're done.

Run:  python examples/quickstart.py
"""
import asyncio

from shardx import ShardX


async def main():
    sdk = ShardX()

    # 1. Create a persistent profile: enriches a random library template (or
    #    pass an id, e.g. "win-rtx4060") with randomized params and freezes it
    #    under a fresh unique id. Do this ONCE per profile.
    profile = sdk.create_profile()
    print("created", profile.id)

    # 1a. Anti-fingerprint noise — declarative: exactly these vectors on, the
    #     rest off. Vectors: canvas, webgl, audio, client_rects, sensors, fonts.
    profile.set_noise("canvas", "audio", "webgl")
    sdk.save_profile(profile)  # persist the choice

    print("saved profiles:", sdk.list_saved_profiles())

    # 2. Launch + drive. randomize defaults False so the frozen fingerprint
    #    stays stable; cookies/cache persist in the profile's folder.
    async with sdk.session(profile) as browser:
        page = await browser.contexts[0].new_page()
        await page.goto("https://example.com")
        print("title:", await page.title())

    # 3. Later (even another process): reopen by id — same fingerprint + state.
    profile = sdk.open_profile(profile.id)

    # 4. Delete when done (wipes config + state).
    # sdk.delete_profile(profile.id)


asyncio.run(main())
