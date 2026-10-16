import assert from "node:assert/strict";
import { test } from "node:test";
import { cookieSourceUrl, describeFailure, originPattern, toCompanionSession } from "./session.js";

test("cookies are collected for the link target, not the page it sits on", () => {
  assert.equal(
    cookieSourceUrl({ pageUrl: "https://a.example/page", mediaUrl: "https://b.example/v.mp4" }),
    "https://b.example/v.mp4",
  );
  assert.equal(cookieSourceUrl({ pageUrl: "https://a.example/page", mediaUrl: null }), "https://a.example/page");
  assert.equal(cookieSourceUrl({ pageUrl: "https://a.example/page", mediaUrl: "  " }), "https://a.example/page");
});

test("host access is requested for one origin and only for http(s)", () => {
  assert.equal(originPattern("https://www.example.com/watch?v=1"), "https://www.example.com/*");
  assert.equal(originPattern("http://localhost:8080/x"), "http://localhost:8080/*");
  assert.equal(originPattern("chrome://extensions"), null);
  assert.equal(originPattern("not a url"), null);
});

test("session maps Chrome cookies to the wire format", () => {
  assert.equal(toCompanionSession([]), null);
  const session = toCompanionSession([
    { name: "sid", value: "v", domain: ".example.com", secure: true, httpOnly: true, expirationDate: 10.9 },
    { name: "t", value: "w", domain: "example.com", path: "/a", secure: false, httpOnly: false },
  ]);
  assert.deepEqual(session.cookies[0], {
    name: "sid",
    value: "v",
    domain: ".example.com",
    path: "/",
    secure: true,
    http_only: true,
    expiration_date: 10,
  });
  assert.equal(session.cookies[1].path, "/a");
  assert.equal(session.cookies[1].expiration_date, 0);
});

test("failure text prefers the host error, then the exception message", () => {
  assert.equal(describeFailure({ ok: false, error: "Phlegethon desktop app is not running" }), "Phlegethon desktop app is not running");
  assert.equal(describeFailure(new Error("Native host has exited.")), "Native host has exited.");
  assert.equal(describeFailure(undefined), "unknown error");
});
