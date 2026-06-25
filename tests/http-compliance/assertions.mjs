/**
 * Assertion engine for iroh-http compliance tests.
 *
 * Given a test case and the actual Response, evaluates all response assertions
 * and returns a result object: { pass: boolean, failures: string[] }
 *
 * Supported assertion fields in case.response:
 *   - status           (number)     exact status code match
 *   - bodyExact        (string)     exact body text match
 *   - bodyNotEmpty     (boolean)    body length > 0
 *   - bodyNot          (string)     body must NOT equal this string
 *   - bodyContains     (string)     body must contain this substring
 *   - bodyMatchesRegex (string)     body must match this regex pattern
 *   - bodyLengthExact  (number)     exact body byte-length match
 *   - bodyMinLength    (number)     body length >= this value
 *   - headers          (object)     each key/value must be present in response headers
 */

/**
 * @param {object} testCase        — a single entry from cases.json
 * @param {Response} res           — the fetch() response
 * @param {string|null} bodyText   — pre-read response body text (null if not read)
 * @param {number|null} bodyLength — pre-computed body byte length (null if not computed)
 * @returns {{ pass: boolean, failures: string[] }}
 */
export function assertResponse(testCase, res, bodyText, bodyLength) {
  const expected = testCase.response;
  const failures = [];

  // status
  if (expected.status !== undefined) {
    if (res.status !== expected.status) {
      failures.push(`status: expected ${expected.status}, got ${res.status}`);
    }
  }

  // bodyExact
  if (expected.bodyExact !== undefined) {
    if (bodyText !== expected.bodyExact) {
      const truncActual = truncate(bodyText, 120);
      const truncExpected = truncate(expected.bodyExact, 120);
      failures.push(
        `bodyExact: expected ${JSON.stringify(truncExpected)}, got ${
          JSON.stringify(truncActual)
        }`,
      );
    }
  }

  // bodyNotEmpty
  if (expected.bodyNotEmpty === true) {
    if (!bodyText || bodyText.length === 0) {
      failures.push(`bodyNotEmpty: body was empty`);
    }
  }

  // bodyNot
  if (expected.bodyNot !== undefined) {
    if (bodyText === expected.bodyNot) {
      failures.push(
        `bodyNot: body must not equal ${JSON.stringify(expected.bodyNot)}`,
      );
    }
  }

  // bodyContains
  if (expected.bodyContains !== undefined) {
    if (!bodyText || !bodyText.includes(expected.bodyContains)) {
      failures.push(
        `bodyContains: body does not contain ${
          JSON.stringify(expected.bodyContains)
        }`,
      );
    }
  }

  // bodyMatchesRegex
  if (expected.bodyMatchesRegex !== undefined) {
    const re = new RegExp(expected.bodyMatchesRegex);
    if (!bodyText || !re.test(bodyText)) {
      failures.push(
        `bodyMatchesRegex: body does not match /${expected.bodyMatchesRegex}/`,
      );
    }
  }

  // bodyLengthExact
  if (expected.bodyLengthExact !== undefined) {
    const len = bodyLength ??
      (bodyText ? new TextEncoder().encode(bodyText).byteLength : 0);
    if (len !== expected.bodyLengthExact) {
      failures.push(
        `bodyLengthExact: expected ${expected.bodyLengthExact}, got ${len}`,
      );
    }
  }

  // bodyMinLength
  if (expected.bodyMinLength !== undefined) {
    const len = bodyLength ?? (bodyText ? bodyText.length : 0);
    if (len < expected.bodyMinLength) {
      failures.push(
        `bodyMinLength: expected >= ${expected.bodyMinLength}, got ${len}`,
      );
    }
  }

  // headers
  if (expected.headers) {
    for (const [name, expectedValue] of Object.entries(expected.headers)) {
      const actual = res.headers.get(name);
      if (actual === null) {
        failures.push(`header ${name}: missing`);
      } else if (actual !== expectedValue) {
        failures.push(
          `header ${name}: expected ${JSON.stringify(expectedValue)}, got ${
            JSON.stringify(actual)
          }`,
        );
      }
    }
  }

  return { pass: failures.length === 0, failures };
}

function truncate(str, maxLen) {
  if (!str) return str;
  if (str.length <= maxLen) return str;
  return str.slice(0, maxLen) + "…";
}
