import 'package:flutter/material.dart';
import '../../builders/value_builder.dart';
import '../../database.dart';

/// Zero-boilerplate adaptive Switch bound directly to a LiliaDB Key-Value boolean.
///
/// Updates instantly in FFI / C ABI memory with Zero-FOUC and emits reactive notifications.
class LiliaKvSwitch extends StatelessWidget {
  final String namespace;
  final String keyName;
  final bool defaultValue;
  final ValueChanged<bool>? onChanged;
  final Color? activeColor;
  final LiliaDatabase? database;

  const LiliaKvSwitch({
    super.key,
    required this.namespace,
    required this.keyName,
    this.defaultValue = false,
    this.onChanged,
    this.activeColor,
    this.database,
  });

  @override
  Widget build(BuildContext context) {
    return LiliaValueBuilder<bool>(
      namespace: namespace,
      keyName: keyName,
      defaultValue: defaultValue,
      database: database,
      builder: (context, value, setValue) {
        return Switch.adaptive(
          value: value,
          activeColor: activeColor,
          onChanged: (newValue) {
            setValue(newValue);
            onChanged?.(newValue);
          },
        );
      },
    );
  }
}
