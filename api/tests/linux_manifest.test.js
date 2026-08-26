const assert = require('node:assert/strict');
const test = require('node:test');

const {
  buildManualVersionEntry,
  buildPlatformPackage,
  loadReleasePolicy,
} = require('../_distribution');


const request = {
  headers: { host: 'updates.example.test', 'x-forwarded-proto': 'https' },
  socket: {},
};
const releaseSource = {
  region: 'global',
  base: 'https://github.com/NiYien/gyroflow/releases/download',
};


function linuxEntry(overrides = {}) {
  return {
    version: '9.9.9',
    tag: 'v9.9.9',
    packages: {
      linux: {
        kind: 'appimage',
        package_filename: 'gyroflow-niyien-linux64.AppImage',
        package_sha256: 'a'.repeat(64),
        package_size: 100,
        archive_filename: 'gyroflow-niyien-linux64.tar.gz',
        archive_sha256: 'b'.repeat(64),
        archive_size: 200,
      },
    },
    ...overrides,
  };
}


test('Linux release package exposes AppImage and tar metadata', () => {
  const result = buildPlatformPackage(request, linuxEntry(), releaseSource, 'linux');

  assert.equal(result.kind, 'appimage');
  assert.ok(result.package_url.endsWith('/v9.9.9/gyroflow-niyien-linux64.AppImage'));
  assert.ok(result.archive_url.endsWith('/v9.9.9/gyroflow-niyien-linux64.tar.gz'));
  assert.equal(result.package_sha256, 'a'.repeat(64));
  assert.equal(result.package_size, 100);
  assert.equal(result.archive_sha256, 'b'.repeat(64));
  assert.equal(result.archive_size, 200);
  const manual = buildManualVersionEntry(linuxEntry(), result, { linux: result });
  assert.equal(manual.url, result.package_url);
});


test('Linux artifact package resolves independent absolute URLs', () => {
  const entry = linuxEntry({
    tag: 'run-42',
    app_source_mode: 'artifact',
    app_urls: {
      linux: {
        package_url: '/api/download/app/run-42/gyroflow-niyien-linux64.AppImage',
        archive_url: '/api/download/app/run-42/gyroflow-niyien-linux64.tar.gz',
      },
    },
  });

  const result = buildPlatformPackage(request, entry, releaseSource, 'linux');

  assert.equal(
    result.package_url,
    'https://updates.example.test/api/download/app/run-42/gyroflow-niyien-linux64.AppImage',
  );
  assert.equal(
    result.archive_url,
    'https://updates.example.test/api/download/app/run-42/gyroflow-niyien-linux64.tar.gz',
  );
});


test('Linux CN package uses separate download routes', () => {
  const result = buildPlatformPackage(request, linuxEntry(), { region: 'cn', base: '' }, 'linux');

  assert.ok(result.package_url.endsWith('/api/download/app/v9.9.9/gyroflow-niyien-linux64.AppImage'));
  assert.ok(result.archive_url.endsWith('/api/download/app/v9.9.9/gyroflow-niyien-linux64.tar.gz'));
});


test('Legacy Linux package defaults to AppImage and omits archive safely', () => {
  const previous = process.env.NIYIEN_RELEASE_POLICY_JSON;
  process.env.NIYIEN_RELEASE_POLICY_JSON = JSON.stringify({
    auto_version: '9.9.9',
    versions: [{
      version: '9.9.9',
      tag: 'v9.9.9',
      channels: ['auto', 'manual'],
      packages: {
        linux: {
          package_filename: 'gyroflow-niyien-linux64.AppImage',
          package_sha256: 'c'.repeat(64),
          package_size: 300,
        },
      },
    }],
  });

  try {
    const entry = loadReleasePolicy().versions[0];
    const result = buildPlatformPackage(request, entry, releaseSource, 'linux');
    assert.equal(result.kind, 'appimage');
    assert.ok(result.package_url.endsWith('/gyroflow-niyien-linux64.AppImage'));
    assert.equal(result.archive_url || '', '');
  } finally {
    if (previous === undefined) delete process.env.NIYIEN_RELEASE_POLICY_JSON;
    else process.env.NIYIEN_RELEASE_POLICY_JSON = previous;
  }
});
