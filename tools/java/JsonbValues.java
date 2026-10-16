import com.fasterxml.jackson.databind.ObjectMapper;
import java.io.BufferedReader;
import java.io.InputStreamReader;
import java.lang.reflect.Method;
import java.nio.charset.StandardCharsets;
import java.util.Properties;
import java.util.Set;
import org.hisp.dhis.attribute.AttributeValues;
import org.hisp.dhis.hibernate.jsonb.type.JsonBinaryType;
import org.hisp.dhis.hibernate.jsonb.type.JsonSetBinaryType;
import org.hisp.dhis.hibernate.jsonb.type.SharingJsonBinaryType;
import org.hisp.dhis.jsontree.JsonBuilder;
import org.hisp.dhis.translation.Translation;
import org.hisp.dhis.common.TranslationProperty;

/**
 * Reads lines "kind \t json" (stdin), converts the JSON as DHIS2's Hibernate JSONB types do and
 * prints "kind \t json \t output" with the output as the Gist API writes such values (a plain
 * ObjectMapper for Jackson-mapped values, json-tree for AttributeValues).
 */
public class JsonbValues {
  static Object convert(JsonBinaryType t, String clazz, String json) throws Exception {
    Properties p = new Properties();
    p.setProperty("clazz", clazz);
    t.setParameterValues(p);
    Method m = JsonBinaryType.class.getDeclaredMethod("convertJsonToObject", String.class);
    m.setAccessible(true);
    // the most specific override (virtual dispatch through the accessible base method)
    return m.invoke(t, json);
  }

  public static void main(String[] args) throws Exception {
    ObjectMapper mapper = new ObjectMapper();
    BufferedReader in =
        new BufferedReader(new InputStreamReader(System.in, StandardCharsets.UTF_8));
    String line;
    while ((line = in.readLine()) != null) {
      if (line.isBlank()) continue;
      String[] kv = line.split("\t", 2);
      String kind = kv[0], json = kv[1];
      String out;
      try {
        switch (kind) {
          case "sharing" -> out =
              mapper.writeValueAsString(
                  convert(new SharingJsonBinaryType(), "org.hisp.dhis.user.sharing.Sharing", json));
          case "translations" -> out =
              mapper.writeValueAsString(
                  convert(new JsonSetBinaryType(), Translation.class.getName(), json));
          case "translationProperty" -> {
            @SuppressWarnings("unchecked")
            Set<Translation> set =
                (Set<Translation>)
                    convert(new JsonSetBinaryType(), Translation.class.getName(), json);
            out = mapper.writeValueAsString(new TranslationProperty(set));
          }
          case "attributeValues" -> {
            AttributeValues av = AttributeValues.of(json);
            out =
                JsonBuilder.createObject(o -> av.addTo("v", o)).getDeclaration().toString()
                    + "\t"
                    + av.toArrayJson();
          }
          default -> out = "?";
        }
      } catch (Exception ex) {
        Throwable c = ex.getCause() != null ? ex.getCause() : ex;
        out = "ERROR " + c.getClass().getSimpleName();
      }
      System.out.println(kind + "\t" + json + "\t" + out);
    }
  }
}
