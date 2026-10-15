// Prints java.util.TimeZone#getDisplayName() (standard time, long, English) for every zone id as
// JSON. Compiled with --release 17 and run with the Java server image's own runtime by
// tools/gen-tz-names.sh, so the names match the JDK in production.
import java.util.Locale;
import java.util.TimeZone;
import java.util.TreeMap;

public class TzNames {
  public static void main(String[] args) {
    TreeMap<String, String> out = new TreeMap<>();
    for (String id : TimeZone.getAvailableIDs()) {
      out.put(id, TimeZone.getTimeZone(id).getDisplayName(false, TimeZone.LONG, Locale.ENGLISH));
    }
    StringBuilder sb = new StringBuilder("{\n");
    int i = 0;
    for (var e : out.entrySet()) {
      sb.append(i++ == 0 ? "" : ",\n").append(" \"").append(e.getKey()).append("\": \"")
          .append(e.getValue().replace("\\", "\\\\").replace("\"", "\\\"")).append('"');
    }
    sb.append("\n}");
    System.out.println(sb);
    System.err.println("java.version=" + System.getProperty("java.version"));
  }
}
