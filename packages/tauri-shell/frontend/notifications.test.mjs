import assert from "node:assert/strict";
import { describe, it, beforeEach } from "node:test";
import {
  loadDesktopNotificationsEnabled,
  saveDesktopNotificationsEnabled,
  shouldShowDesktopNotification,
} from "./notifications.js";

const store = new Map();

beforeEach(() => {
  store.clear();
  globalThis.localStorage = {
    getItem: (key) => store.get(key) ?? null,
    setItem: (key, value) => {
      store.set(key, String(value));
    },
    removeItem: (key) => {
      store.delete(key);
    },
  };
});

describe("desktop notifications", () => {
  it("defaults to enabled when unset", () => {
    localStorage.removeItem("phlegethon-desktop-notifications");
    assert.equal(loadDesktopNotificationsEnabled(), true);
  });

  it("persists disabled preference", () => {
    saveDesktopNotificationsEnabled(false);
    assert.equal(loadDesktopNotificationsEnabled(), false);
    saveDesktopNotificationsEnabled(true);
    assert.equal(loadDesktopNotificationsEnabled(), true);
  });

  it("suppresses when app is focused", () => {
    assert.equal(shouldShowDesktopNotification(true, true), false);
    assert.equal(shouldShowDesktopNotification(true, false), true);
    assert.equal(shouldShowDesktopNotification(false, false), false);
  });
});
