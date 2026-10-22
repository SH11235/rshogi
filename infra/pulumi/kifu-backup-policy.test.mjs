import assert from "node:assert/strict";
import { createRequire } from "node:module";
import { test } from "node:test";
const require = createRequire(import.meta.url);
const { kifuBackupLockRules, kifuBackupLifecycleRules } = require("./kifu-backup-policy.ts");

test("snapshot だけを永久保持し、checkpoint の更新を妨げない", () => {
    assert.equal(kifuBackupLockRules.length, 1);
    const rule = kifuBackupLockRules[0];
    assert.equal(rule.enabled, true);
    assert.equal(rule.prefix, "snapshots/");
    assert.equal(rule.condition.type, "Indefinite");
    assert.equal("control/kifu-backup-v1.json".startsWith(rule.prefix), false);
});

test("棋譜を期限削除する lifecycle rule を作らない", () => {
    for (const rule of kifuBackupLifecycleRules) {
        assert.equal(rule.deleteObjectsTransition, undefined);
        assert.equal(rule.storageClassTransitions, undefined);
        assert.equal(rule.abortMultipartUploadsTransition.condition.maxAge, 604800);
    }
});
