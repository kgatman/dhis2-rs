import java.io.BufferedReader;
import java.io.InputStreamReader;
import java.nio.charset.StandardCharsets;
import java.util.Set;
import java.util.TreeSet;
import org.hisp.dhis.web.tomcat.MimeMappings;
import org.springframework.http.MediaType;
import org.springframework.http.MediaTypeFactory;

/**
 * Prints the file extensions Spring MVC's reflected-file-download check
 * (AbstractMessageConverterMethodProcessor#safeExtension) treats as safe in the DHIS2 server,
 * one per line, sorted. Run with the server image's classpath (tools/gen-rfd-safe-extensions.sh).
 *
 * <p>Safe: SAFE_EXTENSIONS, the content negotiation manager's file extensions (json, xml), and
 * extensions whose media type is audio/*, image/*, video/* or *+xml. The media type comes from
 * the servlet context (DHIS2's Tomcat context uses MimeMappings.DEFAULT) and, when that has none
 * or application/octet-stream, from Spring's MediaTypeFactory (mime.types). Extensions known to
 * neither are unsafe.
 */
public class RfdSafeExtensions {
  public static void main(String[] args) throws Exception {
    Set<String> candidates = new TreeSet<>();
    for (MimeMappings.Mapping m : MimeMappings.DEFAULT) candidates.add(m.getExtension());
    try (BufferedReader r =
        new BufferedReader(
            new InputStreamReader(
                MediaTypeFactory.class.getResourceAsStream("/org/springframework/http/mime.types"),
                StandardCharsets.US_ASCII))) {
      String line;
      while ((line = r.readLine()) != null) {
        if (line.isEmpty() || line.charAt(0) == '#') continue;
        String[] tokens = line.trim().split("\\s+");
        for (int i = 1; i < tokens.length; i++) candidates.add(tokens[i].toLowerCase());
      }
    }
    Set<String> safe =
        new TreeSet<>(
            Set.of(
                "txt", "text", "yml", "properties", "csv", "json", "xml", "atom", "rss", "png",
                "jpe", "jpeg", "jpg", "gif", "wbmp", "bmp"));
    for (String ext : candidates) {
      MediaType type = null;
      String raw = MimeMappings.DEFAULT.get(ext);
      if (raw != null && !raw.isEmpty()) type = MediaType.parseMediaType(raw);
      if (type == null || MediaType.APPLICATION_OCTET_STREAM.equals(type)) {
        type = MediaTypeFactory.getMediaType("file." + ext).orElse(null);
      }
      if (type != null
          && (Set.of("audio", "image", "video").contains(type.getType())
              || type.getSubtype().endsWith("+xml"))) {
        safe.add(ext);
      }
    }
    safe.forEach(System.out::println);
  }
}
