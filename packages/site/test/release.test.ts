import assert from "node:assert/strict";
import test from "node:test";
import { RELEASES_URL, assetName, assetUrl, detectPlatform, type Release } from "../src/release.ts";

const release: Release = {
  tag_name: "v0.1.0-nightly.20261119.9",
  html_url: "https://github.com/kraai-io/kraai/releases/tag/v0.1.0-nightly.20261119.9",
  assets: [
    "kraai-aarch64-apple-darwin.tar.gz",
    "kraai-x86_64-apple-darwin.tar.gz",
    "kraai-aarch64-pc-windows-msvc.zip",
    "kraai-x86_64-pc-windows-msvc.zip",
    "kraai-aarch64-unknown-linux-gnu.tar.gz",
    "kraai-x86_64-unknown-linux-gnu.tar.gz",
    "SHA256SUMS",
  ].map((name) => ({ name, browser_download_url: `https://example.test/${name}` })),
};

test("asset names match the release files", () => {
  assert.equal(assetName("macos", "aarch64"), "kraai-aarch64-apple-darwin.tar.gz");
  assert.equal(assetName("windows", "x86_64"), "kraai-x86_64-pc-windows-msvc.zip");
  assert.equal(assetName("linux", "aarch64"), "kraai-aarch64-unknown-linux-gnu.tar.gz");
});

test("detects the OS and CPU from common user agents", () => {
  const mac = "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/605.1.15 Safari/605.1.15";
  const windows = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 Chrome/142.0 Safari/537.36";
  const linux = "Mozilla/5.0 (X11; Linux x86_64; rv:140.0) Gecko/20100101 Firefox/140.0";
  const linuxArm = "Mozilla/5.0 (X11; Linux aarch64; rv:140.0) Gecko/20100101 Firefox/140.0";
  const iphone = "Mozilla/5.0 (iPhone; CPU iPhone OS 18_0 like Mac OS X) AppleWebKit/605.1.15 Mobile/15E148";

  // Macs report "Intel" even on Apple silicon, so only a client hint sets the CPU.
  assert.deepEqual(detectPlatform(mac), { os: "macos", arch: null });
  assert.deepEqual(detectPlatform(mac, "macOS", "x86"), { os: "macos", arch: "x86_64" });
  assert.deepEqual(detectPlatform(windows), { os: "windows", arch: "x86_64" });
  assert.deepEqual(detectPlatform(windows, "Windows", "arm"), { os: "windows", arch: "aarch64" });
  assert.deepEqual(detectPlatform(linux), { os: "linux", arch: "x86_64" });
  assert.deepEqual(detectPlatform(linuxArm), { os: "linux", arch: "aarch64" });
  assert.deepEqual(detectPlatform(iphone), { os: null, arch: null });
});

test("links to the asset, then the release, then the releases page", () => {
  assert.equal(assetUrl(release, "linux", "x86_64"), "https://example.test/kraai-x86_64-unknown-linux-gnu.tar.gz");
  assert.equal(assetUrl({ ...release, assets: [] }, "linux", "x86_64"), release.html_url);
  assert.equal(assetUrl(null, "macos", "aarch64"), RELEASES_URL);
});
