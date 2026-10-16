import com.fasterxml.jackson.databind.JsonSerializer;
import com.fasterxml.jackson.databind.ObjectMapper;
import com.fasterxml.jackson.databind.ser.BeanPropertyWriter;
import com.fasterxml.jackson.databind.ser.BeanSerializer;
import com.fasterxml.jackson.databind.ser.PropertyWriter;
import java.io.BufferedReader;
import java.io.InputStreamReader;
import java.util.Iterator;
import org.hisp.dhis.commons.jackson.config.JacksonObjectMapperConfig;

/**
 * For each class name on stdin prints one JSON line: the properties DHIS2's JSON mapper writes
 * for it, in serialisation order, with the getter and any custom serializer.
 */
public class JacksonProperties {
  public static void main(String[] args) throws Exception {
    // as FieldFilterService#configureFieldFilterObjectMapper configures its copy
    ObjectMapper mapper = JacksonObjectMapperConfig.staticJsonMapper().copy();
    com.fasterxml.jackson.databind.module.SimpleModule module =
        new com.fasterxml.jackson.databind.module.SimpleModule();
    module.setMixInAnnotation(Object.class, org.hisp.dhis.fieldfiltering.FieldFilterMixin.class);
    mapper.registerModule(module);
    mapper.setAnnotationIntrospector(
        new org.hisp.dhis.fieldfiltering.FieldFilterService
            .IgnoreJsonSerializerRefinementAnnotationInspector());
    BufferedReader in = new BufferedReader(new InputStreamReader(System.in));
    String line;
    StringBuilder out = new StringBuilder("{\n");
    boolean firstClass = true;
    while ((line = in.readLine()) != null) {
      String name = line.trim();
      if (name.isEmpty()) continue;
      Class<?> klass;
      try {
        klass = Class.forName(name);
      } catch (Throwable t) {
        continue;
      }
      JsonSerializer<Object> s;
      try {
        s = mapper.getSerializerProviderInstance().findValueSerializer(klass);
      } catch (Exception e) {
        System.err.println("skipped " + name + ": " + e);
        continue;
      }
      if (!(s instanceof BeanSerializer bs)) continue;
      if (!firstClass) out.append(",\n");
      firstClass = false;
      out.append("  \"").append(name).append("\": [");
      Iterator<PropertyWriter> it = bs.properties();
      boolean first = true;
      while (it.hasNext()) {
        PropertyWriter w = it.next();
        if (!first) out.append(", ");
        first = false;
        out.append("{\"name\": \"").append(w.getName()).append("\"");
        if (w instanceof BeanPropertyWriter b) {
          if (b.getMember() != null) out.append(", \"getter\": \"").append(b.getMember().getName()).append("\"");
          if (b.hasSerializer()) out.append(", \"serializer\": \"").append(b.getSerializer().getClass().getName()).append("\"");
          out.append(", \"type\": \"").append(b.getType().toCanonical().replace("\"", "")).append("\"");
        }
        out.append("}");
      }
      out.append("]");
    }
    out.append("\n}\n");
    System.out.print(out);
  }
}
