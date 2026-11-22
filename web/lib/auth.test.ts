import { describe, expect, it } from "vitest";
import {
  describeAgent,
  passwordProblem,
  safeNext,
  usernameProblem,
} from "./auth";

describe("passwordProblem", () => {
  it("asks for 12 characters, counted as the API counts them", () => {
    expect(passwordProblem("short", "short")).toMatch(/at least 12/);
    // 12 characters, 24 UTF-16 units would pass a naive check either way; 11 must not.
    const eleven = "🔑".repeat(11);
    expect(passwordProblem(eleven, eleven)).toMatch(/at least 12/);
    const twelve = "mật khẩu dài";
    expect([...twelve]).toHaveLength(12);
    expect(passwordProblem(twelve, twelve)).toBeNull();
  });

  it("refuses more than 1024 bytes", () => {
    const long = "é".repeat(513); // 1026 bytes
    expect(passwordProblem(long, long)).toMatch(/1024 bytes/);
  });

  it("wants both fields to match", () => {
    expect(
      passwordProblem("correct horse battery", "correct horse batterY"),
    ).toBe("The two passwords differ.");
  });
});

describe("usernameProblem", () => {
  it("accepts what the API accepts", () => {
    for (const name of ["lan", "Minh.Tran", "ha_2", "a-b", " lan "]) {
      expect(usernameProblem(name), name).toBeNull();
    }
    for (const name of ["", "lan tran", "lan@lab", "minh/x", "x".repeat(65)]) {
      expect(usernameProblem(name), name).not.toBeNull();
    }
  });
});

describe("safeNext", () => {
  it("keeps same-site paths only", () => {
    expect(safeNext("/project/1?x=2")).toBe("/project/1?x=2");
    for (const bad of [null, undefined, "", "https://evil.example", "//evil"]) {
      expect(safeNext(bad)).toBe("/");
    }
  });
});

describe("describeAgent", () => {
  it("names the browser and the system", () => {
    expect(
      describeAgent(
        "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/140.0 Safari/537.36",
      ),
    ).toBe("Chrome on Linux");
    expect(
      describeAgent(
        "Mozilla/5.0 (iPhone; CPU iPhone OS 18_0 like Mac OS X) AppleWebKit/605.1.15 Version/18.0 Mobile/15E148 Safari/604.1",
      ),
    ).toBe("Safari on iOS");
    expect(
      describeAgent(
        "Mozilla/5.0 (Windows NT 10.0; Win64; x64) Chrome/140.0 Safari/537.36 Edg/140.0",
      ),
    ).toBe("Edge on Windows");
    expect(describeAgent("curl/8.5.0")).toBe("curl");
    expect(describeAgent(null)).toBe("Unknown device");
  });
});
