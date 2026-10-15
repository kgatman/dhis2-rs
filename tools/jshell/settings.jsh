// Dumps the setting definitions Java extracts reflectively in org.hisp.dhis.setting.LazySettings
// (defaults, confidential and translatable keys, property names) for SystemSettings and
// UserSettings as JSON. Run by tools/gen-settings.sh.
import org.hisp.dhis.setting.*;
import java.lang.reflect.*;
import com.fasterxml.jackson.databind.ObjectMapper;
@SuppressWarnings("unchecked") void noop() {}
Class<?> lazy = Class.forName("org.hisp.dhis.setting.LazySettings");
Object field(String name) throws Exception { Field f = lazy.getDeclaredField(name); f.setAccessible(true); return f.get(null); }
var confidential = (java.util.Set<String>) field("CONFIDENTIAL_KEYS");
var translatable = (java.util.Set<String>) field("TRANSLATABLE_KEYS");
var keyByProperty = (java.util.Map<String, String>) field("KEY_BY_PROPERTY");
var out = new java.util.TreeMap<String, Object>();
for (String which : new String[] {"DEFAULTS_SYSTEM_SETTINGS", "DEFAULTS_USER_SETTINGS"}) {
  var defaults = (java.util.Map<String, java.io.Serializable>) field(which);
  var table = new java.util.TreeMap<String, Object>();
  for (var e : defaults.entrySet()) {
    var m = new java.util.LinkedHashMap<String, Object>();
    Object d = e.getValue();
    if (d instanceof java.util.Date dt) { m.put("type", "Date"); m.put("default", dt.getTime()); }
    else if (d instanceof Enum<?> en) {
      m.put("type", "Enum"); m.put("default", en.name()); m.put("enumClass", en.getDeclaringClass().getName());
      m.put("enumValues", java.util.Arrays.stream(en.getDeclaringClass().getEnumConstants()).map(c -> ((Enum<?>) c).name()).toList());
      m.put("enumToString", java.util.Arrays.stream(en.getDeclaringClass().getEnumConstants()).map(Object::toString).toList());
    }
    else if (d instanceof java.util.Locale l) { m.put("type", "Locale"); m.put("default", l.toString()); }
    else { m.put("type", d.getClass().getSimpleName()); m.put("default", d); }
    m.put("confidential", confidential.contains(e.getKey()));
    m.put("translatable", translatable.contains(e.getKey()));
    table.put(e.getKey(), m);
  }
  out.put(which.equals("DEFAULTS_SYSTEM_SETTINGS") ? "system" : "user", table);
}
out.put("keyByProperty", new java.util.TreeMap<>(keyByProperty));
System.out.println(new ObjectMapper().writerWithDefaultPrettyPrinter().writeValueAsString(out));
/exit
