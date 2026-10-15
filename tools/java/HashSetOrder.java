import java.util.*;

/**
 * Prints fixtures for the Rust emulation of java.util.HashSet iteration order
 * (crates/dhis-core-model/src/java/hash.rs). Run with the Java runtime of the server image, as
 * HashSet sizing changed between JDK releases (tools/gen-hashset-fixtures.sh).
 *
 * <p>Output: one line per case, tab separated: kind (presized = new HashSet<>(list), incremental =
 * new HashSet<>() then add in order), the input order, the iteration order. Elements are joined
 * with U+001F.
 */
public class HashSetOrder {
  public static void main(String[] args) {
    Random r = new Random(42);
    List<List<String>> inputs = new ArrayList<>();
    // sizes around the JDK 17/21 sizing differences (12, 24, 48) and resize thresholds
    int[] sizes = {0, 1, 2, 7, 11, 12, 13, 16, 23, 24, 25, 47, 48, 49, 55, 96, 97, 200};
    for (int n : sizes) {
      for (int k = 0; k < 4; k++) {
        List<String> l = new ArrayList<>();
        for (int i = 0; i < n; i++) l.add(word(r));
        inputs.add(l);
      }
    }
    // String.hashCode collisions ("Aa" and "BB" hash alike), in both insertion orders
    inputs.add(List.of("Aa", "BB", "x"));
    inputs.add(List.of("BB", "Aa", "x"));
    inputs.add(List.of("AaAa", "BBBB", "AaBB", "BBAa", "q"));
    // non-ASCII (UTF-16 code units, including a surrogate pair)
    inputs.add(List.of("é", "日本", "😀", "plain", "Ω"));
    // duplicates are dropped, first occurrence wins
    inputs.add(List.of("b", "a", "b", "c", "a"));
    // the user setting keys and some authorities
    inputs.add(
        List.of(
            "keyAnalysisDisplayProperty",
            "keyDbLocale",
            "keyMessageEmailNotification",
            "keyMessageSmsNotification",
            "keyStyle",
            "keyTrackerDashboardLayout",
            "keyUiLocale"));
    inputs.add(List.of("ALL", "F_DATAVALUE_ADD", "F_SYSTEM_SETTING", "M_dhis-web-dashboard"));
    for (List<String> in : inputs) {
      print("presized", in, new HashSet<>(in));
      Set<String> inc = new HashSet<>();
      inc.addAll(in);
      print("incremental", in, inc);
    }
  }

  private static String word(Random r) {
    int len = 1 + r.nextInt(30);
    StringBuilder b = new StringBuilder();
    String alphabet = "ABCDEFGHIJKLMNOPQRSTUVWXYZ_abcdefghijklmnopqrstuvwxyz0123456789-";
    for (int i = 0; i < len; i++) b.append(alphabet.charAt(r.nextInt(alphabet.length())));
    return b.toString();
  }

  private static void print(String kind, List<String> in, Set<String> set) {
    System.out.println(
        kind + "\t" + String.join("\u001f", in) + "\t" + String.join("\u001f", set));
  }
}
