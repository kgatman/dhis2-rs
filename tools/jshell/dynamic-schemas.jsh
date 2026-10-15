// Dumps Java property introspection results for the classes in -Dclasses (one JSON line per class).
// Run by tools/gen-dynamic-schemas.sh.
import org.hisp.dhis.schema.*;
import org.hisp.dhis.schema.introspection.*;
import com.fasterxml.jackson.databind.*;
var intro = new JacksonPropertyIntrospector().then(new TranslatablePropertyIntrospector()).then(new PropertyPropertyIntrospector()).then(new GistPropertyIntrospector());
var mapper = new ObjectMapper();
mapper.setSerializationInclusion(com.fasterxml.jackson.annotation.JsonInclude.Include.NON_NULL);
for (String cn : System.getProperty("classes").split(",")) {
  Class<?> k = Class.forName(cn);
  java.util.Map<String, Property> props = new java.util.HashMap<>();
  intro.introspect(k, props);
  var out = new java.util.TreeMap<String, Object>();
  for (var e : props.entrySet()) {
    Property p = e.getValue();
    var m = new java.util.LinkedHashMap<String, Object>();
    m.put("name", p.getName()); m.put("fieldName", p.getFieldName()); m.put("collectionName", p.getCollectionName());
    m.put("klass", p.getKlass() == null ? null : p.getKlass().getName());
    m.put("itemKlass", p.getItemKlass() == null ? null : p.getItemKlass().getName());
    m.put("propertyType", String.valueOf(p.getPropertyType())); m.put("itemPropertyType", String.valueOf(p.getItemPropertyType()));
    m.put("collection", p.isCollection()); m.put("simple", p.isSimple()); m.put("owner", p.isOwner()); m.put("persisted", p.isPersisted());
    m.put("embeddedObject", p.isEmbeddedObject()); m.put("readable", p.isReadable()); m.put("writable", p.isWritable());
    m.put("propertyTransformer", p.hasPropertyTransformer());
    out.put(e.getKey(), m);
  }
  System.out.println(cn + "\t" + mapper.writeValueAsString(out));
}
/exit
