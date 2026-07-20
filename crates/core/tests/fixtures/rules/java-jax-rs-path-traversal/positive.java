class C {
    @GET
    public Response getFile(@PathParam("name") String name) {
        File f = new File(BASE_DIR, name);
        return Response.ok(f).build();
    }
}
