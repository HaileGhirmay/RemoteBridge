import Foundation
import Security

public enum KeyStoreError: Error, Equatable {
    /// `createKey` found an existing key.
    case keyExists
    /// No device key.
    case keyNotFound
    /// A Security.framework call failed with this status.
    case keychain(OSStatus)
    /// Anything else (for example a malformed public key).
    case failed(String)
}

/// Where the private key lives.
public enum KeyBackend: Equatable {
    /// Secure Enclave: hardware-backed, never leaves the chip.
    case secureEnclave
    /// A non-extractable key in the Keychain (Macs without a Secure Enclave,
    /// or processes without the entitlements the Enclave needs).
    case keychain
}

/// Public half of the key as JWK coordinates (base64url, 32 bytes each).
public struct PublicKeyJWK: Equatable {
    public let x: String
    public let y: String
}

/// The device identity key: ECDSA P-256, private half non-exportable.
///
/// Secure Enclave when available, otherwise a non-extractable Keychain key.
/// `SecKeyCreateSignature` returns ASN.1 DER; the Rust core converts it to
/// IEEE P1363 (`DER` is tagged on the way across the bridge).
public final class SecureKeyStore {
    private let tag: Data
    private let preferSecureEnclave: Bool
    private let lock = NSLock()

    /// - Parameters:
    ///   - tag: identifies the key in the Keychain, e.g. `app.remotebridge.devicekey.v1`.
    ///   - preferSecureEnclave: try the Secure Enclave first. `false` forces the Keychain key.
    public init(tag: String = "app.remotebridge.devicekey.v1", preferSecureEnclave: Bool = true) {
        self.tag = Data(tag.utf8)
        self.preferSecureEnclave = preferSecureEnclave
    }

    // MARK: - Public API

    /// The public key, or `nil` if the device has no key.
    public func publicKey() throws -> PublicKeyJWK? {
        lock.lock()
        defer { lock.unlock() }
        guard let privateKey = try lookup() else { return nil }
        return try jwk(for: privateKey)
    }

    /// Create the key. Throws `keyExists` if one exists.
    public func createKey() throws -> PublicKeyJWK {
        lock.lock()
        defer { lock.unlock() }
        if try lookup() != nil { throw KeyStoreError.keyExists }

        if preferSecureEnclave, let key = try? makeKey(secureEnclave: true) {
            return try jwk(for: key)
        }
        return try jwk(for: try makeKey(secureEnclave: false))
    }

    /// ECDSA-SHA256 over `message`, DER encoded.
    public func sign(_ message: Data) throws -> Data {
        lock.lock()
        defer { lock.unlock() }
        guard let privateKey = try lookup() else { throw KeyStoreError.keyNotFound }
        let algorithm = SecKeyAlgorithm.ecdsaSignatureMessageX962SHA256
        guard SecKeyIsAlgorithmSupported(privateKey, .sign, algorithm) else {
            throw KeyStoreError.failed("signing algorithm not supported by this key")
        }
        var error: Unmanaged<CFError>?
        guard let signature = SecKeyCreateSignature(privateKey, algorithm, message as CFData, &error) else {
            throw KeyStoreError.keychain(Self.status(of: error))
        }
        return signature as Data
    }

    /// Delete the key. Used on `DEVICE_REVOKED` and uninstall. Idempotent.
    public func wipe() throws {
        lock.lock()
        defer { lock.unlock() }
        let status = SecItemDelete(baseQuery() as CFDictionary)
        guard status == errSecSuccess || status == errSecItemNotFound else {
            throw KeyStoreError.keychain(status)
        }
    }

    /// Which hardware holds the key, or `nil` if there is no key.
    public func backend() throws -> KeyBackend? {
        lock.lock()
        defer { lock.unlock() }
        guard let privateKey = try lookup() else { return nil }
        let attributes = SecKeyCopyAttributes(privateKey) as? [String: Any]
        let tokenID = attributes?[kSecAttrTokenID as String] as? String
        return tokenID == (kSecAttrTokenIDSecureEnclave as String) ? .secureEnclave : .keychain
    }

    // MARK: - Internals (visible to tests)

    func lookup() throws -> SecKey? {
        var query = baseQuery()
        query[kSecReturnRef as String] = true
        query[kSecMatchLimit as String] = kSecMatchLimitOne
        var item: CFTypeRef?
        let status = SecItemCopyMatching(query as CFDictionary, &item)
        if status == errSecItemNotFound { return nil }
        guard status == errSecSuccess, let item else { throw KeyStoreError.keychain(status) }
        return (item as! SecKey)
    }

    private func baseQuery() -> [String: Any] {
        [
            kSecClass as String: kSecClassKey,
            kSecAttrKeyClass as String: kSecAttrKeyClassPrivate,
            kSecAttrKeyType as String: kSecAttrKeyTypeECSECPrimeRandom,
            kSecAttrApplicationTag as String: tag,
        ]
    }

    private func makeKey(secureEnclave: Bool) throws -> SecKey {
        var privateAttributes: [String: Any] = [
            kSecAttrIsPermanent as String: true,
            kSecAttrApplicationTag as String: tag,
        ]
        var attributes: [String: Any] = [
            kSecAttrKeyType as String: kSecAttrKeyTypeECSECPrimeRandom,
            kSecAttrKeySizeInBits as String: 256,
            kSecPrivateKeyAttrs as String: privateAttributes,
        ]

        if secureEnclave {
            var accessError: Unmanaged<CFError>?
            // Usable after the first unlock so unattended start at login can sign;
            // never synced or migrated to another device.
            guard let access = SecAccessControlCreateWithFlags(
                nil,
                kSecAttrAccessibleAfterFirstUnlockThisDeviceOnly,
                .privateKeyUsage,
                &accessError
            ) else {
                throw KeyStoreError.keychain(Self.status(of: accessError))
            }
            privateAttributes[kSecAttrAccessControl as String] = access
            attributes[kSecPrivateKeyAttrs as String] = privateAttributes
            attributes[kSecAttrTokenID as String] = kSecAttrTokenIDSecureEnclave
        } else {
            // Software key: make sure it can never be exported.
            attributes[kSecAttrIsExtractable as String] = false
        }

        var error: Unmanaged<CFError>?
        guard let key = SecKeyCreateRandomKey(attributes as CFDictionary, &error) else {
            throw KeyStoreError.keychain(Self.status(of: error))
        }
        return key
    }

    private func jwk(for privateKey: SecKey) throws -> PublicKeyJWK {
        guard let publicKey = SecKeyCopyPublicKey(privateKey) else {
            throw KeyStoreError.failed("no public key")
        }
        var error: Unmanaged<CFError>?
        guard let data = SecKeyCopyExternalRepresentation(publicKey, &error) as Data? else {
            throw KeyStoreError.keychain(Self.status(of: error))
        }
        // ANSI X9.63 uncompressed point: 0x04 || X (32) || Y (32).
        guard data.count == 65, data.first == 0x04 else {
            throw KeyStoreError.failed("unexpected public key encoding")
        }
        return PublicKeyJWK(
            x: Self.base64url(data.subdata(in: 1..<33)),
            y: Self.base64url(data.subdata(in: 33..<65))
        )
    }

    static func base64url(_ data: Data) -> String {
        data.base64EncodedString()
            .replacingOccurrences(of: "+", with: "-")
            .replacingOccurrences(of: "/", with: "_")
            .replacingOccurrences(of: "=", with: "")
    }

    private static func status(of error: Unmanaged<CFError>?) -> OSStatus {
        guard let error = error?.takeRetainedValue() else { return errSecInternalError }
        return OSStatus(CFErrorGetCode(error))
    }
}
