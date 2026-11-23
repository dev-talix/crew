import { expect, test } from "bun:test";
import { draftTitle, MAILTO_LIMIT, mailtoUrl, parseRecipients } from "./draftText";

test("splits typed recipients", () => {
  expect(parseRecipients(" a@example.com, b@example.com;\nc@example.com ,, ")).toEqual(["a@example.com", "b@example.com", "c@example.com"]);
  expect(parseRecipients("  ")).toEqual([]);
});

test("builds a mailto link with every part encoded", () => {
  const { url, bodyIncluded } = mailtoUrl(["a+b@example.com", "c@example.com"], " Q3 plan & budget? ", "Hi,\nSee below.\n100% done");
  expect(bodyIncluded).toBe(true);
  expect(url).toBe("mailto:a%2Bb@example.com,c@example.com?subject=Q3%20plan%20%26%20budget%3F&body=Hi%2C%0D%0ASee%20below.%0D%0A100%25%20done");
  expect(mailtoUrl(["a@example.com"], "", "").url).toBe("mailto:a@example.com");
  // A recipient can't smuggle in extra fields.
  expect(mailtoUrl(["x@example.com?bcc=evil@example.com"], "", "").url).toBe("mailto:x@example.com%3Fbcc%3Devil@example.com");
});

test("leaves a long body out of the link", () => {
  const { url, bodyIncluded } = mailtoUrl(["a@example.com"], "Report", "x".repeat(MAILTO_LIMIT));
  expect(bodyIncluded).toBe(false);
  expect(url).toBe("mailto:a@example.com?subject=Report");
});

test("names a draft by channel", () => {
  expect(draftTitle("email", ["a@example.com", "b@example.com"])).toBe("Email to a@example.com, b@example.com");
  expect(draftTitle("slack", ["#eng"])).toBe("Slack to #eng");
  expect(draftTitle("post", ["LinkedIn"])).toBe("Post for LinkedIn");
  expect(draftTitle("post", [])).toBe("Post");
});
