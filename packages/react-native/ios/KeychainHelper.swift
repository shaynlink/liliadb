import Foundation
import Security

public enum KeychainHelper {
    private static let serviceName = "io.liliadb.secure_storage"

    public static func getOrCreateKey(forDatabaseName name: String) throws -> Data {
        let accountName = "db_key_\(name)"

        // 1. Try to load existing key
        let query: [String: Any] = [
            kSecClass as String: kSecClassGenericPassword,
            kSecAttrService as String: serviceName,
            kSecAttrAccount as String: accountName,
            kSecReturnData as String: true,
            kSecMatchLimit as String: kSecMatchLimitOne
        ]

        var item: CFTypeRef?
        let status = SecItemCopyMatching(query as CFDictionary, &item)

        if status == errSecSuccess, let keyData = item as? Data, keyData.count == 32 {
            return keyData
        }

        // 2. Generate new 256-bit (32 bytes) key
        var newKey = Data(count: 32)
        let result = newKey.withUnsafeMutableBytes { ptr in
            SecRandomCopyBytes(kSecRandomDefault, 32, ptr.baseAddress!)
        }
        guard result == errSecSuccess else {
            throw NSError(domain: "io.liliadb.keychain", code: -1, userInfo: [NSLocalizedDescriptionKey: "Failed to generate random 256-bit key"])
        }

        // 3. Save key to Keychain
        let attributes: [String: Any] = [
            kSecClass as String: kSecClassGenericPassword,
            kSecAttrService as String: serviceName,
            kSecAttrAccount as String: accountName,
            kSecValueData as String: newKey,
            kSecAttrAccessible as String: kSecAttrAccessibleAfterFirstUnlockThisDeviceOnly
        ]

        // Delete any outdated key with same name
        SecItemDelete(query as CFDictionary)
        SecItemAdd(attributes as CFDictionary, nil)

        return newKey
    }
}
