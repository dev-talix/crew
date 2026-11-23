import { expect, test } from "bun:test";
import { acceptFiles, formatSize, isImage, MAX_FILES, MAX_IMAGE_BYTES } from "./attachments";

const file = (name: string, size = 10, type = "application/pdf") => ({ name, size, type });
const png = (name: string, size = 10) => file(name, size, "image/png");

test("tells images the models accept from other files", () => {
  expect(isImage(png("a.png"))).toBe(true);
  expect(isImage(file("a.jpg", 1, "image/jpeg"))).toBe(true);
  expect(isImage(file("a.heic", 1, "image/heic"))).toBe(false);
  expect(isImage(file("a.pdf"))).toBe(false);
});

test("keeps at most ten files", () => {
  const current = Array.from({ length: 8 }, (_, i) => file(`f${i}`));
  const { accepted, error } = acceptFiles(current, [file("a"), file("b"), file("c")]);
  expect(accepted.map((f) => f.name)).toEqual(["a", "b"]);
  expect(error).toBe(`Attach up to ${MAX_FILES} files at a time.`);
  expect(acceptFiles(Array.from({ length: 10 }, () => file("x")), [file("y")]).accepted).toEqual([]);
  expect(acceptFiles([], [file("a")])).toEqual({ accepted: [file("a")], error: "" });
});

test("refuses images over 20 MB but not big files", () => {
  const big = png("huge.png", MAX_IMAGE_BYTES + 1);
  const { accepted, error } = acceptFiles([], [big, png("ok.png", MAX_IMAGE_BYTES), file("video.mov", MAX_IMAGE_BYTES * 3)]);
  expect(accepted.map((f) => f.name)).toEqual(["ok.png", "video.mov"]);
  expect(error).toBe("huge.png is over 20 MB. Images can be up to 20 MB.");
  expect(acceptFiles([], [big, png("b.png", MAX_IMAGE_BYTES + 5)]).error).toBe("2 images are over 20 MB. Images can be up to 20 MB.");
});

test("formats sizes", () => {
  expect(formatSize(512)).toBe("512 B");
  expect(formatSize(2048)).toBe("2 KB");
  expect(formatSize(3.46 * 1024 * 1024)).toBe("3.5 MB");
  expect(formatSize(42 * 1024 * 1024)).toBe("42 MB");
});
