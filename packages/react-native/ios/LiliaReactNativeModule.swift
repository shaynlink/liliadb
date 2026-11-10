import Foundation
import React

#if canImport(Lilia)
import Lilia
#endif

@objc(LiliaReactNativeModule)
public class LiliaReactNativeModule: NSObject {
    private var instances = [String: LiliaDatabase]()
    private let lock = NSLock()

    @objc
    public static func requiresMainQueueSetup() -> Bool {
        return false
    }

    private func resolveProfile(_ profileStr: String) -> FfiResourceProfile {
        switch profileStr.lowercased() {
        case "constrained":
            return .constrained
        case "server":
            return .server
        default:
            return .mobile
        }
    }

    @objc
    public func openDatabase(
        _ options: NSDictionary,
        resolver resolve: @escaping RCTPromiseResolveBlock,
        rejecter reject: @escaping RCTPromiseRejectBlock
    ) {
        guard let path = options["path"] as? String else {
            reject("INVALID_INPUT", "Path is required", nil)
            return
        }

        let profileStr = (options["profile"] as? String) ?? "mobile"
        let profile = resolveProfile(profileStr)
        let useSecureStorage = (options["useSecureStorage"] as? Bool) ?? false

        var encKeyData: Data? = nil

        if useSecureStorage {
            do {
                let dbName = URL(fileURLWithPath: path).lastPathComponent
                encKeyData = try KeychainHelper.getOrCreateKey(forDatabaseName: dbName)
            } catch {
                reject("KEYCHAIN_ERROR", "Failed to retrieve or generate Keychain key: \(error)", error)
                return
            }
        } else if let hex = options["encryptionKeyHex"] as? String, hex.count == 64 {
            var data = Data(capacity: 32)
            var index = hex.startIndex
            for _ in 0..<32 {
                let nextIndex = hex.index(index, offsetBy: 2)
                let byteString = hex[index..<nextIndex]
                if let byte = UInt8(byteString, radix: 16) {
                    data.append(byte)
                }
                index = nextIndex
            }
            if data.count == 32 {
                encKeyData = data
            }
        }

        DispatchQueue.global(qos: .userInitiated).async {
            do {
                let db: LiliaDatabase
                if let passphrase = options["passphrase"] as? String {
                    db = try LiliaDatabase.openWithPassphrase(path: path, profile: profile, passphrase: passphrase)
                } else {
                    db = try LiliaDatabase.open(path: path, profile: profile, encryptionKey: encKeyData)
                }

                let handle = UUID().uuidString
                self.lock.lock()
                self.instances[handle] = db
                self.lock.unlock()

                resolve(["handle": handle, "path": path])
            } catch {
                reject("OPEN_ERROR", "Failed to open database: \(error)", error)
            }
        }
    }

    private func getDb(_ handle: String) -> LiliaDatabase? {
        lock.lock()
        defer { lock.unlock() }
        return instances[handle]
    }

    @objc
    public func prepareForBackground(
        _ handle: String,
        resolver resolve: @escaping RCTPromiseResolveBlock,
        rejecter reject: @escaping RCTPromiseRejectBlock
    ) {
        guard let db = getDb(handle) else {
            reject("NOT_FOUND", "Database handle not found", nil)
            return
        }
        do {
            try db.prepareForBackground()
            resolve(true)
        } catch {
            reject("FLUSH_ERROR", error.localizedDescription, error)
        }
    }

    // ==========================================
    // KEY / VALUE OPERATIONS
    // ==========================================

    @objc
    public func kvSet(
        _ handle: String,
        namespace: String,
        keyHex: String,
        valHex: String,
        expiresAtMs: NSNumber?,
        resolver resolve: @escaping RCTPromiseResolveBlock,
        rejecter reject: @escaping RCTPromiseRejectBlock
    ) {
        guard let db = getDb(handle) else {
            reject("NOT_FOUND", "Database handle not found", nil)
            return
        }
        let key = Data(hexString: keyHex)
        let val = Data(hexString: valHex)
        let expires = expiresAtMs?.int64Value

        Task {
            do {
                let version = try await db.kvSet(namespace: namespace, key: key, value: val, expiresAtMs: expires)
                resolve(NSNumber(value: version))
            } catch {
                reject("KV_ERROR", error.localizedDescription, error)
            }
        }
    }

    @objc
    public func kvGet(
        _ handle: String,
        namespace: String,
        keyHex: String,
        resolver resolve: @escaping RCTPromiseResolveBlock,
        rejecter reject: @escaping RCTPromiseRejectBlock
    ) {
        guard let db = getDb(handle) else {
            reject("NOT_FOUND", "Database handle not found", nil)
            return
        }
        let key = Data(hexString: keyHex)

        Task {
            do {
                if let entry = try await db.kvGet(namespace: namespace, key: key) {
                    resolve([
                        "namespace": entry.namespace,
                        "keyHex": entry.key.hexString,
                        "valueHex": entry.value.hexString,
                        "version": NSNumber(value: entry.version),
                        "expiresAtMs": entry.expiresAtMs.map { NSNumber(value: $0) } as Any
                    ])
                } else {
                    resolve(nil)
                }
            } catch {
                reject("KV_ERROR", error.localizedDescription, error)
            }
        }
    }

    // ==========================================
    // JSON OPERATIONS
    // ==========================================

    @objc
    public func jsonPut(
        _ handle: String,
        space: String,
        id: String,
        valueJson: String,
        resolver resolve: @escaping RCTPromiseResolveBlock,
        rejecter reject: @escaping RCTPromiseRejectBlock
    ) {
        guard let db = getDb(handle) else {
            reject("NOT_FOUND", "Database handle not found", nil)
            return
        }
        Task {
            do {
                let version = try await db.jsonPut(space: space, id: id, valueJson: valueJson)
                resolve(NSNumber(value: version))
            } catch {
                reject("JSON_ERROR", error.localizedDescription, error)
            }
        }
    }

    @objc
    public func jsonGet(
        _ handle: String,
        space: String,
        id: String,
        resolver resolve: @escaping RCTPromiseResolveBlock,
        rejecter reject: @escaping RCTPromiseRejectBlock
    ) {
        guard let db = getDb(handle) else {
            reject("NOT_FOUND", "Database handle not found", nil)
            return
        }
        Task {
            do {
                if let doc = try await db.jsonGet(space: space, id: id) {
                    resolve([
                        "space": doc.space,
                        "id": doc.id,
                        "valueJson": doc.valueJson,
                        "version": NSNumber(value: doc.version)
                    ])
                } else {
                    resolve(nil)
                }
            } catch {
                reject("JSON_ERROR", error.localizedDescription, error)
            }
        }
    }

    // ==========================================
    // SQL OPERATIONS
    // ==========================================

    @objc
    public func sqlQuery(
        _ handle: String,
        sql: String,
        paramsJson: String?,
        resolver resolve: @escaping RCTPromiseResolveBlock,
        rejecter reject: @escaping RCTPromiseRejectBlock
    ) {
        guard let db = getDb(handle) else {
            reject("NOT_FOUND", "Database handle not found", nil)
            return
        }
        Task {
            do {
                let res = try await db.sqlQuery(sql: sql, paramsJson: paramsJson)
                resolve([
                    "columns": res.columns,
                    "rowsJson": res.rowsJson,
                    "rowsAffected": NSNumber(value: res.rowsAffected)
                ])
            } catch {
                reject("SQL_ERROR", error.localizedDescription, error)
            }
        }
    }

    // ==========================================
    // CLOSE
    // ==========================================

    @objc
    public func close(
        _ handle: String,
        timeoutMs: NSNumber,
        resolver resolve: @escaping RCTPromiseResolveBlock,
        rejecter reject: @escaping RCTPromiseRejectBlock
    ) {
        guard let db = getDb(handle) else {
            resolve(true)
            return
        }
        lock.lock()
        instances.removeValue(forKey: handle)
        lock.unlock()

        Task {
            do {
                try await db.close(timeoutMs: timeoutMs.uint64Value)
                resolve(true)
            } catch {
                reject("CLOSE_ERROR", error.localizedDescription, error)
            }
        }
    }
}

fileprivate extension Data {
    init(hexString: String) {
        var data = Data(capacity: hexString.count / 2)
        var index = hexString.startIndex
        while index < hexString.endIndex {
            let nextIndex = hexString.index(index, offsetBy: 2, limitedBy: hexString.endIndex) ?? hexString.endIndex
            let byteString = hexString[index..<nextIndex]
            if let byte = UInt8(byteString, radix: 16) {
                data.append(byte)
            }
            index = nextIndex
        }
        self = data
    }

    var hexString: String {
        return map { String(format: "%02hhx", $0) }.joined()
    }
}
