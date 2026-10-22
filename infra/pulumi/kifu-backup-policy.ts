export const kifuBackupLockRules = [{
    id: "retain-kifu-snapshots",
    prefix: "snapshots/",
    enabled: true,
    condition: { type: "Indefinite" },
}];

export const kifuBackupLifecycleRules = [{
    id: "abort-incomplete-multipart",
    enabled: true,
    conditions: { prefix: "" },
    abortMultipartUploadsTransition: {
        condition: { maxAge: 7 * 24 * 60 * 60, type: "Age" },
    },
}];
