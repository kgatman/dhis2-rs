import java.util.ArrayList;
import java.util.List;
import java.util.Random;
import org.hisp.dhis.jsontree.JsonBuilder;

/**
 * Prints how json-tree writes doubles: "bits \t TextBuilder form \t streamed form". The
 * TextBuilder form is what `JsonBuilder.createArray` produces (Gist's unwrapped lists), the
 * streamed form what `JsonBuilder.streamArray` writes (everything else).
 */
public class JsonTreeDoubles {
  public static void main(String[] args) throws Exception {
    List<Double> values = new ArrayList<>(List.of(
        0.0, -0.0, 1.0, -1.0, 0.5, -0.5, 0.05, -0.05, 1.05, -1.05, 1.25, -18.75, 55.0, 2.5,
        0.1, 0.2, 0.3, 0.123, 0.1234, 0.001, 0.0009, 9999999.0, 1e7, 1e-4, 123.456, 1.1, 2.675,
        100.01, 3.14159, -0.001, 1234567.891, 0.007, 10.1, 10.01, 10.001, 1.0E10, Double.NaN,
        Double.POSITIVE_INFINITY, Double.NEGATIVE_INFINITY, Double.MIN_VALUE, Double.MAX_VALUE));
    Random r = new Random(42);
    for (int i = 0; i < 300; i++) values.add(Math.round(r.nextGaussian() * 100000) / 1000d);
    for (int i = 0; i < 100; i++) values.add(r.nextDouble() * 20 - 10);
    for (double v : values) {
      String tb = JsonBuilder.createArray(arr -> arr.addNumber(v)).getDeclaration().toString();
      java.io.ByteArrayOutputStream out = new java.io.ByteArrayOutputStream();
      JsonBuilder.streamArray(out, arr -> arr.addNumber(v));
      String streamed = out.toString(java.nio.charset.StandardCharsets.UTF_8);
      System.out.println(Double.doubleToRawLongBits(v) + "\t" + tb.substring(1, tb.length() - 1)
          + "\t" + streamed.substring(1, streamed.length() - 1));
    }
  }
}
