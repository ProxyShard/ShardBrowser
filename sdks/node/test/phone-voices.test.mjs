// A phone offers the handset's voices, not the machine the launcher runs on.
import { test } from "node:test";
import assert from "node:assert/strict";
import { applyPhoneVoices, ANDROID_SPEECH_VOICES } from "../dist/profile.js";

test("a phone without voices gets the handset's", () => {
  const cfg = {
    navigator: { user_agent: "Mozilla/5.0 (Linux; Android 14; SM-S921B) Mobile Safari/537.36" },
  };
  applyPhoneVoices(cfg);
  const voices = cfg.speech.voices;
  assert.equal(voices.length, ANDROID_SPEECH_VOICES.length);
  // Chrome's rule: the first voice in the list is the default one.
  assert.equal(voices[0].is_default, true);
  assert.equal(voices[0].name, "Google US English");
  assert.ok(voices.slice(1).every((v) => v.is_default === false));
  // Android's voices come from Google TTS, so none of them is on the device.
  assert.ok(voices.every((v) => v.local_service === false));
  assert.ok(voices.every((v) => typeof v.name === "string" && v.name && typeof v.lang === "string" && v.lang));
});

test("a desktop profile is left alone", () => {
  const cfg = {
    navigator: { user_agent: "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) Safari/537.36" },
    speech: { voices: [{ name: "Samantha", lang: "en-US", local_service: true, is_default: true }] },
  };
  applyPhoneVoices(cfg);
  assert.deepEqual(cfg.speech.voices.map((v) => v.name), ["Samantha"]);
});

test("a phone that declares its own voices keeps them", () => {
  const cfg = {
    client_hints: { mobile: true },
    speech: { voices: [{ name: "English (United States)", lang: "en-US", local_service: true, is_default: true }] },
  };
  applyPhoneVoices(cfg);
  assert.deepEqual(cfg.speech.voices.map((v) => v.name), ["English (United States)"]);
});

test("an empty voice list is not a claim", () => {
  // The engine reads an empty list as none and enumerates the host's voices
  // anyway, so a phone carrying one still needs the roster.
  const cfg = { client_hints: { mobile: true }, speech: { voices: [] } };
  applyPhoneVoices(cfg);
  assert.equal(cfg.speech.voices.length, ANDROID_SPEECH_VOICES.length);
});
