import Security
import XCTest
@testable import RemoteBridgeMac

final class SecureKeyStoreTests: XCTestCase {
    /// A store with a unique tag that is wiped when the test ends. Skips the
    /// test where the Keychain cannot be used at all (for example a headless
    /// CI user with no usable keychain), rather than reporting a false failure.
    private func makeStore(secureEnclave: Bool) throws -> SecureKeyStore {
        let store = SecureKeyStore(
            tag: "app.remotebridge.test.\(UUID().uuidString)",
            preferSecureEnclave: secureEnclave
        )
        addTeardownBlock { try? store.wipe() }
        return store
    }

    private func createOrSkip(_ store: SecureKeyStore) throws -> PublicKeyJWK {
        do {
            return try store.createKey()
        } catch KeyStoreError.keychain(let status) {
            throw XCTSkip("Keychain unavailable here (OSStatus \(status))")
        }
    }

    /// Rebuild a verifying key from the JWK coordinates, like the server does.
    private func verifyingKey(_ jwk: PublicKeyJWK) throws -> SecKey {
        func decode(_ s: String) -> Data {
            var b64 = s.replacingOccurrences(of: "-", with: "+").replacingOccurrences(of: "_", with: "/")
            while b64.count % 4 != 0 { b64 += "=" }
            return Data(base64Encoded: b64)!
        }
        var point = Data([0x04])
        point.append(decode(jwk.x))
        point.append(decode(jwk.y))
        let attributes: [String: Any] = [
            kSecAttrKeyType as String: kSecAttrKeyTypeECSECPrimeRandom,
            kSecAttrKeyClass as String: kSecAttrKeyClassPublic,
            kSecAttrKeySizeInBits as String: 256,
        ]
        var error: Unmanaged<CFError>?
        let key = SecKeyCreateWithData(point as CFData, attributes as CFDictionary, &error)
        return try XCTUnwrap(key, "public key from JWK must be a valid P-256 point")
    }

    func testKeyLifecycleAndSignatureVerifies() throws {
        let store = try makeStore(secureEnclave: false)
        XCTAssertNil(try store.publicKey())
        XCTAssertNil(try store.backend())
        XCTAssertThrowsError(try store.sign(Data("x".utf8))) {
            XCTAssertEqual($0 as? KeyStoreError, .keyNotFound)
        }

        let jwk = try createOrSkip(store)
        XCTAssertEqual(jwk.x.count, 43, "32 bytes in base64url")
        XCTAssertEqual(jwk.y.count, 43)
        XCTAssertEqual(try store.publicKey(), jwk)
        XCTAssertThrowsError(try store.createKey()) {
            XCTAssertEqual($0 as? KeyStoreError, .keyExists)
        }

        let message = Data("RA-HOST-V1\nhost/poll".utf8)
        let der = try store.sign(message)
        XCTAssertEqual(der.first, 0x30, "SecKeyCreateSignature answers in DER")
        XCTAssertLessThanOrEqual(der.count, 72)

        var error: Unmanaged<CFError>?
        let valid = SecKeyVerifySignature(
            try verifyingKey(jwk),
            .ecdsaSignatureMessageX962SHA256,
            message as CFData,
            der as CFData,
            &error
        )
        XCTAssertTrue(valid, "signature must verify against the exported public key")

        try store.wipe()
        try store.wipe()
        XCTAssertNil(try store.publicKey())
    }

    func testPrivateKeyCannotBeExported() throws {
        let store = try makeStore(secureEnclave: false)
        _ = try createOrSkip(store)
        let privateKey = try XCTUnwrap(try store.lookup())

        var error: Unmanaged<CFError>?
        let exported = SecKeyCopyExternalRepresentation(privateKey, &error)
        XCTAssertNil(exported, "private key export must fail")
        error?.release()
    }

    func testPreferringSecureEnclaveStillWorksAndReportsBackend() throws {
        let store = try makeStore(secureEnclave: true)
        let jwk = try createOrSkip(store)
        let backend = try XCTUnwrap(try store.backend())
        print("device key backend on this machine: \(backend)")

        let message = Data("m".utf8)
        let der = try store.sign(message)
        var error: Unmanaged<CFError>?
        XCTAssertTrue(
            SecKeyVerifySignature(
                try verifyingKey(jwk),
                .ecdsaSignatureMessageX962SHA256,
                message as CFData,
                der as CFData,
                &error
            )
        )
        // Hardware or software, the private key must stay inside.
        var exportError: Unmanaged<CFError>?
        XCTAssertNil(SecKeyCopyExternalRepresentation(try XCTUnwrap(try store.lookup()), &exportError))
        exportError?.release()
    }

    func testDifferentTagsAreDifferentKeys() throws {
        let a = try makeStore(secureEnclave: false)
        let b = try makeStore(secureEnclave: false)
        let ja = try createOrSkip(a)
        let jb = try createOrSkip(b)
        XCTAssertNotEqual(ja, jb)
        XCTAssertNil(try SecureKeyStore(tag: "app.remotebridge.test.unused").publicKey())
    }
}
