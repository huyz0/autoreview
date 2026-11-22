import java.time.format.DateTimeFormatter;
class CalendarFormat {
    DateTimeFormatter format() { return DateTimeFormatter.ofPattern("uuuu-MM-dd"); }
    DateTimeFormatter week() { return DateTimeFormatter.ofPattern("YYYY-ww-e"); }
}
